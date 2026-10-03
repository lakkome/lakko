<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0065: Reset account keys: a server-attested key generation that signs out and removes every other device

- **Status:** Proposed (2026-10-03). **Phase 1 built on 2026-10-03** on the
  server (`services/auth`, `services/discovery`, `services/ds`), both FFI
  surfaces and all three clients (web, iOS, Android); **unit-tested, not
  walked live** on any device or against a deployed server, and no passkey
  ceremony was executed. The verification observed by the coordinator that
  day: auth DB-gated tests 120 + 2 green over three runs; discovery 107 green
  over three runs; DS 85 green over two runs; web `tsc` + vitest 3914 +
  `vite build`; iOS `LakkoTests` 2613 on a simulator with the core rebuilt;
  Android host unit tests 2997 (the native core is not loaded there and the
  `.so` was not rebuilt). The deviations from the text below are collected
  in "What the build changed / found". **Phase 2 is unbuilt**: it touches
  `lakko-mls` and sits under G-39; its residual (the old identity's leaves
  stay in conversation groups until peers evict them) is tracked as
  **G-44**, the stolen-session reset residual of section 2 as **G-45**. The
  maintainer's decisions of 2026-10-03 (what a reset must do) are recorded
  as given; the mechanisms are this document's and are built as written
  except where the build section says otherwise.
- **Date:** 2026-10-03
- **Context:** account recovery, Part 4 (after [ADR-0017](0017-phrase-seeded-identity.md),
  [ADR-0018](0018-recovery-factor-cascade.md), [ADR-0049](0049-identity-recovery-word-vault.md),
  [ADR-0064](0064-recovery-model-simplification.md))
- **Amends:** [ADR-0014](0014-webauthn-action-binding.md) (a passkey deletion
  without an action grant), [ADR-0017](0017-phrase-seeded-identity.md)
  (`mintFreshIdentity` becomes server-attested, atomic on the server, and
  its client order changes), [ADR-0050](0050-per-device-conversation-leaves.md)
  (section 2.10's "server-side session revocation on removal is arc C" lands
  for the reset case; the Devices list), [ADR-0053](0053-ds-journal-authorisation.md)
  (the DS verifies one auth-signed artefact), [ADR-0057](0057-journal-ownership.md)
  (a second way to replace a journal root; the "same root re-claims" rule no
  longer holds after a key reset), [ADR-0064](0064-recovery-model-simplification.md)
  (decision 9's routes fold into the reset; open question 7 answered for the
  reset case). [ADR-0037](0037-backup-head-rollback-commitment.md) is
  examined and **not** amended (the backup-head section). Supersedes none of them.
- **UX spec:** [`docs/ux/identity-recovery.md`](../ux/identity-recovery.md)
  section 15 (added with this ADR).

Number check: `ls docs/adr` on 2026-10-03 in this worktree showed 0064 as
the highest file, so 0065 is free here. Other branches were not checked.

## Context

### What "Reset account keys" is

The interface calls it **Reset account keys**; the code calls it
`mintFreshIdentity` (ADR-0017). It is reached after ACCESS, on a device
that holds no keys, when no route to the keys is left: the person mints a
new recovery phrase, derives a new `AccountIdentity` from it, and publishes
the new identity bundle without a supersession proof. ADR-0017 calls this
"the single sanctioned publish-without-proof"; ADR-0064 decision 9 added the
recovery-credential replace after it.

### The maintainer's decisions (2026-10-03)

When a person resets the account keys:

1. every **other** device of the account is signed out automatically and
   removed from the account;
2. the account's **passkeys are deleted** (the authenticator / TOTP stays;
   section 2.4 records the reasoning and flags it);
3. the signed-out device **wipes everything local** (keys, phrase, chats,
   contacts), not only the keys;
4. two phases: phase 1 is the server reset operation, session revocation,
   the old device's sign-out and wipe, no republish-back, the directory
   rows and the DeviceGroup re-founding; phase 2 is the eviction of the
   old-identity leaves from conversation groups by the peers. One ADR
   covers both; phase 1 is built first.

### Facts about the code (read on 2026-10-03 unless marked)

The recon of 2026-10-02 (the facts file this ADR was written from) was
re-verified line by line where a decision below rests on it; lines marked
"(recon)" are carried from it without a re-read today.

**Session tokens.**

1. A session token is `v1.<account_id_b64url>.<expires_at_ms>.<tag>`
   (`crates/lakko-auth-token/src/lib.rs:9-28`): account id and expiry only,
   no device, no generation, no issued-at. The crate doc says tokens are
   "stateless in Phase 1 - no DB row, no revocation list. The 30-day TTL is
   the only invalidation path" (`lib.rs:49-55`); `VERSION = "v1"` with a
   note that a `v2` "adds fields" (`lib.rs:89-93`). `issue` at `:155`,
   `verify` at `:191` (constant-time tag, then expiry).
2. TTL: `DEFAULT_SESSION_TOKEN_TTL_DAYS = 30`
   (`services/auth/src/main.rs:148`), env `LAKKO_SESSION_TOKEN_TTL_DAYS`
   (`:293-300`). `issue_token_for` (`services/auth/src/opaque.rs:125-144`)
   is `now + ttl`. There is no refresh route and no revocation in the route
   table (`main.rs:488-737`).
3. Verifiers hold only the shared `LAKKO_SESSION_TOKEN_SECRET` and none can
   read auth's database: auth itself (`webauthn::require_session`,
   `services/auth/src/webauthn.rs:911-950`, which returns a bare
   `StatusCode` with no body, layered on the passkey / account / action /
   device / TOTP / vault / login-handle / recovery-credential routes
   `main.rs:578-581`, `/call/jwt` `:590`, `/call/turn-credential` `:604`,
   `/auth/genesis/attest` `:617`, `/auth/backup/head` `:629`,
   `/auth/tokens` `:640`); discovery (`services/discovery/src/auth.rs:62-106`,
   one uniform `401` body); storage (`services/storage/src/auth.rs:68`, on
   the `/backup` routes `main.rs:186-197`); gif-proxy (recon). Storage and
   gif-proxy have no database at all (neither `Cargo.toml` depends on
   `sqlx`). The DS, push-gw and channels never see a session token
   (ADR-0053 proofs, anonymous tokens, owner signatures).
4. Clients never refresh; silent resume reuses the persisted token while it
   is unexpired (recon: web `auth/silent-restore.ts:73-101`, iOS
   `AuthRepository.swift:290-323`, Android `AuthRepository.kt:355-390`).
5. After `/recovery/credential/finish` the old device can still obtain a
   session with the password (the password envelope is untouched,
   `opaque.rs:1478-1479`, the upsert at `:1514-1523` names `KIND_RECOVERY`
   only), a passkey, TOTP or a device grant (recon). Only the old phrase
   stops working.

**Devices of an account, server side.**

6. Discovery holds one KeyPackage row per `(account_id, device_id)`,
   `device_id` being the device leaf's Ed25519 key
   (`services/discovery/migrations/0017_keypackages_per_device.sql`),
   at most 8 per account (`services/discovery/src/keypackage.rs:57`).
   Routes: `POST /discovery/keypackage`, `DELETE
   /discovery/keypackage/device/{device_id}` (`services/discovery/src/main.rs:439-444`),
   public `GET /discovery/keypackage/{account_id}` (`:605`). The delete
   needs any session of the account and no device proof
   (`keypackage.rs:224-254`). A deleted row makes the other devices retire
   that leaf from chats (recon: web `chat/device-readmission.ts:55-84`).
7. The DeviceGroup id is `BLAKE3("lakko-device-group/v1" || account_id)`
   (`crates/lakko-mls/src/device_group.rs:31,154,180`): the **same** id
   before and after a key reset. `is_active()` at `:571`.
8. DS journal ownership (ADR-0057): the root seed is
   `BLAKE3-derive-key("lakko/ds/journal-root/v1", account_identity_seed ||
   group_id)` (`docs/adr/0057-journal-ownership.md:58-62`,
   `crates/lakko-ds-proof/src/statement.rs:74,113`), so it hangs on the
   account seed. In `services/ds/src/ownership.rs`, `decide` hears a
   `Claim` only from an unclaimed journal or the root, and `Reset` /
   `Registrars` / `RootTransfer` only from the root (`:286-293`); a
   stranger gets the uniform `401`. `apply_claim` by the root is idempotent
   when it names what is there and otherwise `409` (`:555-583`);
   `apply_reset` deletes the epoch chain and re-registers (`:586-611`).
   **So only a device holding the OLD seed can reset, transfer or register
   epochs on the DeviceGroup journal and on every group journal the account
   owns.**
9. push-gw: registrations and group subscriptions are keyed by a random
   device pseudonym, anonymous, with nothing linking them to an account;
   the server cannot list an account's devices (recon; by design,
   ADR-0042).
10. Auth passkeys: `webauthn_credentials (credential_id, account_id)`;
    `DELETE /webauthn/credentials/{id}` requires an ADR-0014 action grant
    when the credential exists (`webauthn.rs:288-323`) and deletes the
    passkey's vault blob in the same transaction (`:324-330`). `DELETE
    /account` requires the grant when the account has a passkey
    (`:368-390`). `DELETE /vault/passkey` needs only a session
    (`services/auth/src/passkey_vault.rs:229-247`). After a mint the vaults
    still hold the OLD phrase entropy: no vault call inside the mint
    functions (recon).
11. `accounts` is `(id, created_at, identity_version BIGINT UNSIGNED
    DEFAULT 1, flags)` (`services/auth/migrations/0001_init.sql:19-27`;
    no later migration alters it, `grep identity_version
    services/auth/migrations` finds only 0001). `identity_version` is read
    by the account export (`webauthn.rs:423-467`) and never bumped.
12. The recovery-credential replace of ADR-0064 decision 9:
    `/recovery/credential/{start,finish}` (`main.rs:570-577`, handlers
    `opaque.rs:1448-1540`), charged to `limits::Scope::RecoveryReplace = 12`
    (`services/auth/src/limits.rs:166`, default `Window::new(10, 3600)`
    `:220`); `finish` is one transaction that deletes the in-flight
    recovery-login and password-reset states and upserts the
    `KIND_RECOVERY` envelope (`opaque.rs:1504-1529`).
13. `lakko-attest` holds four fixed-layout Ed25519 kinds signed by auth's
    shared attest key: device-attest (`crates/lakko-attest/src/lib.rs:120,136`),
    genesis (`genesis.rs:66,86`), backup head (`backup_head.rs:91,118`),
    release manifest, plus the username certificate. Auth loads the attest
    key at `webauthn_action::load_or_init_attest_key`
    (`services/auth/src/webauthn_action.rs:270`), publishes it at
    `GET /.well-known/device-attest-jwks.json` (`main.rs:718-721`) and the
    clients pin it build-time (ADR-0015).

**What `mintFreshIdentity` does today** (recon; web `AuthContext.tsx:2635-2830`
re-read at `:2635-2660` today; iOS `AuthRepository.swift:2485-2575`; Android
`AuthRepository.kt:2236-2358`): generate the phrase and seed; publish the new
bundle with a genesis `leaf_auth` and a genesis attestation; bootstrap or
reuse the device key; cache the new seed, persist the session token, store
the phrase, mark the phrase owed; replace the recovery credential or record
it as owed; park `save-fresh-phrase`. It does **not** found a new
DeviceGroup, delete other devices' directory rows, touch the old DeviceGroup
journal, remove push registrations, remove passkeys or vaults, or touch
conversation groups. On the next session start the minting device runs
`ensureDeviceGroup -> createFounder`, whose claim with the NEW root meets the
old root's claim and gets `401` (recon: web `devices/lifecycle.ts:140-191`,
`ds/ownership.ts:316-330`). By fact 8 it can never succeed. The new identity
is in no chat.

**What an old device experiences today.**

14. `verifySeedAgainstPublishedIdentity` runs only when a seed arrives
    (phrase, pairing, device restore), never on a silent restore or a warm
    password / passkey login with a cached seed (recon).
15. **The publish-back.** On landing every client compares its cached seed
    with the published bundle, raises a dismissible banner when they differ,
    and then **re-uploads its own (old) bundle**: web
    `refreshPublishedIdentity` (`apps/web/src/auth/AuthContext.tsx:962-1001`,
    re-read today; the alert at `:983`, the upload at `:996`), called from
    `finishCanonicalSession` (`:1201,1211`); Android and iOS do the same on
    sign-in (recon). On the server, `cooldown_outcome`
    (`services/discovery/src/identity_bundle.rs:653-672`) treats
    `previous == new_sig` as a heal that is never gated, and the update at
    `:578-599` swaps current and previous; the comment at `:473-475` calls
    the result "a flip-flop". **So the old device reverts the reset.**
16. No client has global `401` handling; a `401` maps to per-feature copy
    (recon). DS `401`s drive the ADR-0053 / ADR-0057 lazy claim and repair,
    not a sign-out.
17. "This device was removed" triggers only on DeviceGroup
    `isActive == false` (`apps/web/src/devices/sync/removed-device.ts:37-54`,
    re-read), is a blocking screen with one **Sign out** button that runs
    forget-device, with catalog keys `devices.removed.{title,body,sign_out}`
    (`l10n/catalog/en.json:4335-4349`, all `security_critical`). `docs/ux/second-device.md`
    section 3.1 records that a device cut off by a DeviceGroup reset reads
    `401`, not a removal, and is unhandled.
18. `forgetDevice` (recon: web `AuthContext.tsx:3559-3645` ->
    `wipe-account.ts:149-249`; iOS `AppServices.forgetThisDevice` ->
    `AccountLocalDataWipe.wipe`; Android `ChatRepository.forgetDevice` +
    `AuthRepository.forgetDevice`) releases the wake binding, deletes its
    own directory KeyPackage, stops the writers, wipes the device key, MLS
    snapshot, message log, session token, export key, both seeds, the
    DeviceGroup snapshot, contacts, the DS root / registrar state and the
    restore marker, unsubscribes push, drops in-memory secrets, forgets the
    anonymous tokens, and lands anonymous.

### Hard constraints that follow

- Tokens are stateless and verified by four services from one shared
  secret: revocation needs either a token-format change plus a floor every
  verifier can apply, or short tokens with a database-checked refresh on
  auth (fact 3).
- The old device can always sign in again: password, passkey, TOTP and a
  device grant all survive a mint (fact 5).
- The old device reverts the identity, and the server accepts it as a heal
  (fact 15).
- The old seed owns the DeviceGroup journal and the account's group journals
  (fact 8).
- The DS, push-gw and channels never check session tokens; the old device
  keeps receiving group traffic while it remains an MLS member (fact 3).
- The only "removed" UX hangs off `isActive == false`, and the wipe runs
  only on a tap (fact 17).
- Passkey vaults keep the old phrase entropy after a mint, and deleting a
  passkey needs a passkey action grant (fact 10).

## Decision

**A reset is one atomic server operation that bumps a per-account key
generation, and the generation is what signs the other devices out.** Every
session token carries the generation it was issued under; auth checks it
against the database on every bearer request and on a new refresh route;
the other verifiers keep verifying statelessly and are reached only by
short-lived access tokens, so their revocation latency is the access TTL.
The reset returns an auth-signed **key-reset attestation** that discovery
(the identity bundle and the directory) and the DS (the DeviceGroup
journal) require before they let the new identity replace the old one, and
discovery no longer accepts an unattested change of the published key at
all, so nothing can publish the old identity back. A device whose session
is refused as superseded shows one blocking screen and wipes itself. Peers
evicting the old identity's leaves from conversation groups is phase 2.

### Phase 1

#### 1. The key generation (`services/auth`)

- **Column:** reuse `accounts.identity_version` (fact 11). Its 0001 comment
  already describes it ("monotonic counter for KT-log version bumps on
  identity rotation"), its default of 1 is the genesis generation, nothing
  bumps it today, and `/account/export` already reports it, which becomes
  meaningful. No column migration. The code calls it `key_generation`; the
  column keeps its name. (Alternative considered: a rename migration for
  clarity. A one-line `ALTER TABLE ... RENAME COLUMN` is cheap, but the
  export route's JSON field name would change with it; the maintainer may
  prefer the rename, see open question 2.)
- **Semantics:** generation `g` names the set of credentials and devices
  that belong to the account after its `(g-1)`-th reset. A session token,
  the published identity bundle, the directory rows and the DeviceGroup
  journal each record the generation they belong to. Anything of a lower
  generation is refused by the party that records it (sections 2, 4, 5).

#### 2. The reset operation: `POST /account/keys/reset/{start,finish}` (bearer)

Two round trips, because replacing the recovery credential is an OPAQUE
registration (ADR-0064 decision 9) and that is two messages. The existing
`/recovery/credential/{start,finish}` routes are **folded in**: `reset/start`
is byte-for-byte `recovery/credential/start` (stateless, charged to the same
counter), and `reset/finish` is `recovery/credential/finish` plus everything
below in the same transaction. The standalone pair is deleted: after this
ADR the only reason to replace the recovery credential is a reset. (ADR-0064
decision 9 is amended accordingly; its retry state, "recovery credential
owed", disappears, because the credential and the generation move in one
transaction and the thing that can still be owed afterwards is the publish,
section 2.3.)

- **`POST /account/keys/reset/start {recovery_request}`** ->
  `{recovery_response}`. As today's `recovery_credential_start`
  (`opaque.rs:1448-1466`).
- **`POST /account/keys/reset/finish {recovery_upload, new_signature_pubkey_b64}`**
  -> `{key_generation, key_reset_attest_b64, session_token,
  session_expires_at_ms, refresh_token, refresh_expires_at_ms}`.
  One transaction, in this order:
  1. `UPDATE accounts SET identity_version = identity_version + 1 WHERE id
     = ?` and read the new value `g` (the MariaDB `LAST_INSERT_ID` idiom
     ADR-0037's `backup_head_versions` already uses, so the read and the
     bump are one statement);
  2. the recovery-credential replace exactly as `opaque.rs:1504-1529`
     (delete the in-flight recovery-login and password-reset states, upsert
     `KIND_RECOVERY`);
  3. `DELETE FROM passkey_vaults WHERE account_id = ?`, then `DELETE FROM
     webauthn_credentials WHERE account_id = ?` (blob first, then the
     credential, the order `delete_credential` uses), plus the account's
     rows in the WebAuthn state and action-grant tables (`webauthn_states`,
     `webauthn_action_grants`), so no half-finished ceremony of the old
     generation can complete;
  4. `DELETE FROM device_grants WHERE account_id = ?` and `DELETE FROM
     device_requests WHERE account_id = ?`: a grant minted by an old device
     before the reset must not land a fresh device in a session that then
     fails the published-identity check with the old entropy (ADR-0064
     decision 8); cutting it at the server is cleaner than letting it fail
     closed on the client;
  5. mint the attestation (section 2.2) over `(account_id, g,
     new_signature_pubkey, now)`;
  6. issue the caller a new access token and refresh credential of
     generation `g` (section 3).
  Commit. The caller's own old token is now superseded too; the response
  carries its replacement, so the client swaps tokens before its next
  request.
- **Kept, deliberately:** the password envelope (`KIND_PASSWORD`) and the
  TOTP secret. The password is the credential the person just used to get
  here; a reset that invalidated it would lock out the one path that
  worked. TOTP lives in an authenticator app, not on a Lakko device, so
  signing out Lakko devices does not touch it, and it is an ACCESS factor
  that cannot yield keys (ADR-0018). **Flagged for the maintainer (open
  question 1):** an attacker who holds the password or the authenticator
  can sign in again after the reset and reach "Reset account keys"
  themselves, which they could before this ADR as well; the reset narrows
  nothing here and widens nothing. The login handle, the anonymous-token
  quota rows, and the backup-head counter are kept (they are per account,
  not per generation).
- **No ADR-0014 action grant**, even when the account has a passkey. The
  route exists for the lost-everything path, where the passkey may be gone
  too (ADR-0064 decision 9's reasoning); and a grant would be asked of a
  session that, before this ADR, could already mint a fresh identity. **The
  residual, stated:** a stolen session can reset the account keys, which it
  could already do with `mintFreshIdentity`, and now also strips every
  passkey and signs out every other device, bounded by the same per-account
  limit (`Scope::RecoveryReplace`, both calls charged, default 10 per hour).
  What changes for the owner is that the theft is now **loud**: every other
  device of theirs shows the signed-out screen within the access TTL
  instead of silently flip-flopping the directory. A tighter scope for the
  reset itself is open question 3.
- **Errors:** `400` malformed; `401` (section 3's codes); `429
  too-many-attempts` with `retry_after_secs`; `503 rate-limiter-unavailable`
  (closed, as today); `503 key-reset-disabled` when the attest key is not
  loaded (it always is today: `load_or_init_attest_key` runs at boot).

##### 2.2 The key-reset attestation (`lakko-attest`, a fifth kind)

Fixed layout, own domain separator, signed with the shared attest key,
verified against the pinned key, in the family of device-attest, genesis
and backup-head:

```text
version(1)=0x01 ‖ account_ref(32) ‖ device_group_journal_id(32)
  ‖ key_generation(8 BE u64) ‖ new_signature_pubkey(32)
  ‖ signed_at_unix_millis(8 BE i64) ‖ sig(64)                 = 177 bytes
DOMAIN = b"lakko/key-reset-attest/v1"
signing input = DOMAIN ‖ version ‖ account_ref ‖ journal_id ‖ generation ‖ pubkey ‖ signed_at
account_ref = BLAKE3-derive-key("lakko/key-reset/account-ref/v1", account_id)
device_group_journal_id = BLAKE3("lakko-device-group/v1" ‖ account_id)   (fact 7, computed by auth)
```

Why these fields, and why not the brief's `account_id ‖ generation ‖
signed_at ‖ sig`:

- **`account_ref` instead of `account_id`.** The DS must verify this blob
  (section 5), and ADR-0053 rejected putting the session token on the DS
  because it names the account (invariant #5). A blob carrying `account_id`
  would do the same. `account_ref` is a one-way per-account value only
  auth and discovery can relate to an account; discovery checks it against
  the session's `account_id` by re-deriving it, the DS never learns the
  account id. It is a fresh domain rather than
  `lakko_transparency::account_username_hash` so the DS cannot link its
  journal to a public KT-log entry either. The DS learns one stable
  per-account pseudonym from it; the DeviceGroup journal id in the request
  path already is one, so nothing new is linkable.
- **`device_group_journal_id`.** Carried so the DS can check it against the
  path without computing it from an account id it does not have. Auth
  computes it from `account_id` with the `lakko-mls` derivation of fact 7
  (the derivation is a public rule; auth learning which DS journal is the
  account's DeviceGroup gives it nothing it could not compute before).
- **`new_signature_pubkey`.** Binds the attestation to the identity the
  reset installs, exactly as ADR-0036's genesis attestation binds the
  OPAQUE-authenticated account to a client-asserted key: stated plainly,
  this is **a binding of the authenticated account to a client-asserted
  key at generation `g`**, not an independent proof the key is right. It
  stops a holder of the attestation from presenting a different key under
  it, and lets discovery refuse a bundle whose key is not the attested one.
- **`signed_at`** for audit and for the one freshness check that exists
  (section 4: a client-side bound on how old an attestation may be when it
  is first applied). The DS and discovery do not check freshness: the
  generation's strict monotonicity makes a replay harmless (an applied
  generation cannot be applied again), and a reset upload may legitimately
  be retried days after the mint (section 2.3).
- Two verify entry points, as the genesis kind: `verify_key_reset_binding`
  (signature, all fields, no freshness) for discovery and the DS, and the
  same with a caller-supplied freshness window for a client. Fuzz target
  `key_reset_attestation_open`. A five-way domain-crossing test joins the
  existing four-way one.

Discovery stores the blob opaque in the identity-bundle row (section 4);
the DS stores only the generation (section 5). Neither serves the blob in
phase 1; peers learn "reset" from the KT log's `is_reset`, which discovery
already threads (`identity_bundle.rs:311-335`).

##### 2.3 The client order

Today the bundle is published first so that a failure changes nothing
locally (ADR-0064 decision 9). With an attested publish the server reset
must come first, and the point of no return moves to `reset/finish`:

1. Generate the phrase and the seed; derive the new identity.
2. `reset/start`, `reset/finish`. On success the account is at generation
   `g`, every other device is being signed out, and the caller holds the
   attestation and the new tokens. **Persist immediately**, before any
   further network call: the new seed and phrase under the device key, the
   new tokens, the attestation, and a `publish-owed` marker (the successor
   of ADR-0064's `recovery-credential-owed`).
3. `POST /discovery/identity` with the new bundle, the genesis `leaf_auth`
   and genesis attestation as today, **plus** `key_reset_attest`
   (section 4). On success clear `publish-owed`.
4. `PUT /ds/group/{device_group}/reset` with the attestation (section 5),
   then found the new DeviceGroup under the same id and bind a fresh push
   routing id (ADR-0042; the old binding was dropped by the reset).
5. Re-publish this device's KeyPackage. Its device binding must be signed
   by the **new** account key (ADR-0050 section 2.2: the binding is
   `account_key`-signed, so every binding of the old generation is stale
   after a reset). Whether each client's KeyPackage publisher re-derives
   the binding from the session identity on every publish, or caches a
   signed binding that would now be stale, **was not verified** and is a
   build-time check (sprint S6).
6. Park `save-fresh-phrase` and show the words (UX 9.1).

A crash or a network failure after step 2 leaves a device that holds the
new seed, the attestation and `publish-owed`; the next landing retries
steps 3 to 5 (the attestation is accepted as long as its generation is
above the one discovery / the DS recorded, which it is until the retry
succeeds). The words are withheld while `publish-owed` is set, as 9.1
withholds them today, because a phrase for an identity nobody can find is
not yet a recovery phrase. A failure **in** step 2 changes nothing: `start`
stores nothing, `finish` is one transaction, and the client discards the
generated phrase.

#### 3. Session revocation: short access tokens, a refresh credential only auth accepts

**Decision: variant (b).** Compared with (a), a `v2` token carrying the
generation that every verifier checks against a floor:

| | (a) generation in the token, floor at every verifier | (b) short access token + auth-checked refresh |
|---|---|---|
| Services changed | auth, discovery, storage, gif-proxy (token format + a floor each) and a way to feed the floor to three services with no database (storage, gif-proxy have none, fact 3) | auth (refresh route, generation check); discovery gains a floor it derives from its own stored generation (section 4), no feed needed; storage and gif-proxy unchanged |
| Revocation latency | immediate once the floor reaches the verifier; the feed is the hard part | at auth immediate (DB check per bearer request); elsewhere at most the access TTL |
| Clients | new token format; no refresh logic | new token format, a refresh credential, a token provider that refreshes ahead of expiry, and the superseded-code handling |
| What the old device can do in the window | nothing, once the floor has arrived; everything, until it has | section 3.4 |

(b) is chosen: it confines the stateful check to the one service that has
the database, needs no cross-service floor feed, and bounds the window by a
knob. The token still carries the generation (so discovery can apply the
floor it already holds, section 4, and so a client knows its own
generation), which takes the useful half of (a) without its feed.

##### 3.1 Formats (`lakko-auth-token`)

- **Access token (v2), flag day, v1 refused:**
  `v2.<account_id_b64url>.<key_generation>.<expires_at_ms>.<tag_b64url>`,
  the tag over the four earlier segments; `verify` returns
  `{account_id, key_generation}`. Every verifier (auth, discovery, storage,
  gif-proxy) moves to v2 in one deploy; there are no users and no tokens to
  keep valid (`docs/PRODUCT-READINESS.md`).
- **Refresh credential:**
  `r1.<account_id_b64url>.<key_generation>.<expires_at_ms>.<tag_b64url>`,
  the tag computed with a key derived from the shared secret,
  `BLAKE3-derive-key("lakko/session-refresh/v1", secret)`, so an access
  verifier's `verify` refuses it (`UnsupportedVersion("r1")`) and the
  access tag can never be mistaken for a refresh tag even if someone
  removed the version check. No random id and no database row: a refresh
  credential is revoked by the generation, never individually (per-device
  revocation is a rejected alternative, section 8).
- **TTLs (chosen, not measured):** access `LAKKO_SESSION_TOKEN_TTL_SECS`,
  default 900 (15 minutes); refresh `LAKKO_REFRESH_TOKEN_TTL_DAYS`, default
  30 (today's session TTL). `LAKKO_SESSION_TOKEN_TTL_DAYS` is removed. The
  access TTL is the revocation latency at discovery (for everything but the
  two writes section 4 gates), storage and gif-proxy, and the upper bound
  on how often a foreground client refreshes; 15 minutes is a first value
  to measure against the test stack's auth load, not a result.

##### 3.2 Routes (`services/auth`)

- **`POST /auth/session/refresh {refresh_token}`** (anonymous; the refresh
  credential is the authorisation) -> `{session_token,
  session_expires_at_ms, refresh_token, refresh_expires_at_ms,
  key_generation}`. Verifies the credential (tag, version, expiry), then
  reads `accounts.identity_version`: equal -> issue a new access token and
  a new refresh credential (sliding: a fresh 30 days); lower -> `401
  {"error":"key-generation-superseded"}`; no account -> `401
  {"error":"refresh-invalid"}` (the same body as a bad tag or an expired
  credential, so an attacker cannot tell a deleted account from a wrong
  token). Per-address limited (`limits::Category::Attempt`).
- **Every login route** (`/login/finish`, `/recovery/login/finish`,
  `/webauthn/auth/finish`, `/totp/recover`, `/device/authorize/redeem`) and
  `/signup/finish` return the pair `{session_token, session_expires_at_ms,
  refresh_token, refresh_expires_at_ms}` plus `key_generation`, issued at
  the account's current generation (one indexed read in `issue_token_for`).
- **`webauthn::require_session`** (the auth middleware) additionally reads
  `accounts.identity_version` and refuses a token whose generation is lower
  with `401 {"error":"key-generation-superseded"}`; a missing account is the
  plain `401`. One primary-key read per bearer request; auth's bearer
  routes are low-volume (passkey management, call JWT, attestation mints,
  token issuance). Discovery's, storage's and gif-proxy's middleware keep
  their stateless verify and their uniform `401`.

##### 3.3 The client token provider

Today each client hands its REST clients a closure returning the persisted
token (web `auth.identitySecrets().sessionToken`; iOS `{
authRepository.identitySecrets?.sessionToken }`; Android
`authRepository.identitySecrets.value?.sessionToken`; per the recon and
the CLAUDE.md `apps/` notes). It becomes a `SessionTokenProvider`:

- `current()` returns the access token when more than 120 s (chosen) remain,
  else awaits a refresh; one refresh in flight per account, callers queue
  behind it.
- A refresh that answers `key-generation-superseded` raises
  **superseded** (section 3.5). `refresh-invalid` (expired or unknown)
  lands on the sign-in screen with the existing "session expired" copy and
  **no wipe**: the keys are still this account's. A network error keeps the
  current token (the caller fails as it does today when offline).
- Any bearer route of any service answering `401` makes the caller try
  **one** refresh, then retry once; a second `401` is reported as today.
  This replaces the absent global `401` handling (fact 16) with one rule.
- Silent restore (fact 4) refreshes instead of reusing the persisted
  access token; offline, the app opens on local data and every server call
  fails as it does today after an expiry.
- The refresh credential is persisted where the session token is persisted
  today (web IndexedDB under the device key, iOS Keychain, Android
  `EncryptedSharedPreferences`; recon) and wiped with it.

##### 3.4 What the old device can still do during the window

For at most the access TTL after the reset (or until it next refreshes):

- **auth:** nothing; every bearer route checks the generation. No call JWT,
  no TURN credential, no backup head, no anonymous tokens, no passkey
  management.
- **discovery:** reads and the writes section 4 does not gate (invite and
  call-invite ledgers, channel registration, public records). The identity
  bundle and the KeyPackage directory are gated by the stored generation
  the moment the new device's attested upload lands (section 4), which in
  the normal flow is seconds after the reset.
- **storage:** `PUT /backup`. The old device cannot mint a head
  (ADR-0037, auth refuses), so it can only re-put a blob it already holds:
  a headless legacy blob or a framed blob with a head of version `<= N`.
  The backup-head section argues this is bounded and visible, not silent.
- **gif-proxy:** searches. Harmless.
- **DS, push-gw, channels:** unchanged by tokens. The old device keeps
  reading every conversation journal in which it still holds a leaf, until
  phase 2 evicts it, and keeps its push-gw rows until it wipes. Its
  DeviceGroup epoch keys are deleted by the attested DS reset (section 5),
  so it reads `401` there at once.
- **Anonymous tokens it already holds** (ADR-0054, one-day epochs) keep
  spending at discovery lookups, DS knocks and push-gw registration until
  the epoch ends; they are not account-bound and cannot name the account.

##### 3.5 The old device signs out and wipes

On `key-generation-superseded` from the refresh route, or from any bearer
route of auth or discovery:

1. The client stops every writer and every poll loop, as the
   removed-device state does (`DeviceGroupSyncContext` stop,
   `pollStepAfterBatch -> "stop-removed"`, fact 17), and shows the blocking
   screen **"Your account keys were reset on another device"** (UX
   section 15.2), reusing the `RemovedDeviceScreen` component with a
   second variant. No dismiss, no chat list behind it.
2. It runs the **full forget-device wipe automatically** (fact 18), with two
   differences from the tapped flow:
   - it makes **no call that needs a session or the old root**: the
     KeyPackage delete (already done by the reset, and the token is
     superseded) and the DS wake-binding release (the old root is a
     stranger to the re-founded journal) are skipped;
   - it **publishes nothing first**: no bundle, no KeyPackage, no DeviceGroup
     message. The screen is shown before the wipe starts so a wipe that
     takes seconds is not a blank app.
   It does unsubscribe its push-gw rows (anonymous tokens, best effort),
   because nothing else can tear them down (G-36). Wiped: the device key,
   the MLS snapshot, the message log, the session and refresh tokens, the
   export key, both seeds, the phrase store, the DeviceGroup snapshot,
   contacts, the DS root / registrar state, the restore marker, the
   three-word budget and suspension marker, the anonymous tokens, in-memory
   secrets: everything `wipeAccountLocalData` and its native twins wipe.
3. After the wipe the screen shows one button, **Sign in**, which goes to
   the anonymous sign-in screen. Signing in again lands the device as a
   device with no keys (ADR-0064's "Restore your keys" prompt), where
   another device of generation `g` can hand it the new keys.

The same screen is also what a device shows when its own landing finds its
keys stale (section 4, rule 4.2), so there is one state, "this device's
keys are not the account's keys any more", reached two ways.

#### 4. No publish-back

##### 4.1 Server (`services/discovery`)

The identity bundle row gains `key_generation BIGINT UNSIGNED NOT NULL
DEFAULT 1` and an opaque `key_reset_attest VARBINARY(256) NULL` sidecar
(migration, flag day: every existing row reads generation 1).

Rules on `POST /discovery/identity`:

- A key **change** (`signature_pubkey` differs from the stored one) is
  accepted only when the upload carries **either** a supersession proof
  (ADR-0016, dormant but kept) **or** a key-reset attestation whose
  `account_ref` is the session account's, whose `new_signature_pubkey` is
  the uploaded key, and whose generation is **strictly greater** than the
  stored one. An unattested, unproven key change is refused `409
  {"error":"identity-change-needs-attestation"}`. The ADR-0017 "heal"
  (`is_heal` in `cooldown_outcome`, `identity_bundle.rs:661`) is retired:
  it was the mechanism of the publish-back, and with every reset attested
  there is no legitimate unattested change left to heal from. The cooldown
  stays as defence in depth on attested changes (a stolen session resetting
  repeatedly is already bounded at auth; discovery's window bounds row
  churn).
- An attested change stores the attestation, sets `key_generation`, and
  **deletes every KeyPackage row of the account** (fact 6) in the same
  transaction. Not "every row but the resetting device's": the resetting
  device's own row carries a binding signed by the old account key
  (section 2.3 step 5) and must be re-published anyway. The resetting
  device re-publishes right after (step 5).
- `POST /discovery/identity` and `POST /discovery/keypackage` refuse a
  token whose generation is lower than the row's `key_generation` with
  `401 {"error":"key-generation-superseded"}` (the one place discovery can
  name the cause: the floor is its own row, not a feed). A token of a
  higher generation than the row is accepted (the attested upload has not
  landed yet); it raises nothing. Every other discovery route keeps the
  stateless verify.
- `GET /discovery/identity/{account_id}` (the fetch) adds `key_generation`
  to its response, so a client can compare the published generation with
  its own.

The ledger delete lives server-side on the attested upload rather than as
a client-called `DELETE /discovery/keypackage/others`: it is one
transaction with the key change, it needs no new route, and a client that
crashes between the two cannot leave other devices' rows standing under a
new identity.

##### 4.2 Client rule, on every landing that holds a cached seed

Before any upload, on every session landing including a silent restore and
a warm password or passkey login with a cached seed (fact 14 names the
paths that run no check today):

1. Fetch the published bundle. A fetch failure skips the upload (as the
   silent-reload mode does today, `AuthContext.tsx:988`) and lands the
   session; the next landing checks again.
2. **Published key equals the cached key:** land; upload only where the
   client uploads today (explicit sign-in; the silent-reload "unchanged"
   skip stays).
3. **Published key differs and the published generation is above the
   device's** (the generation from its own token): this device holds stale
   keys. Show the section 3.5 screen and wipe. Never upload.
4. **Published key differs at the same generation:** impossible under 4.1
   except through a proven rotation (ADR-0016, dormant). Show the blocking
   screen with the line "This device's keys do not match the account's
   published identity" and the **Sign out** button of the removed-device
   screen; the wipe runs on the tap, not automatically, because the state
   is an anomaly rather than a known reset. Never upload. The dismissible
   `published-identity-changed` banner (fact 15) and the heal upload that
   followed it are **deleted**.
5. **No bundle published** (404): as today (the device publishes).

The attestation a device applies on its own reset (section 2.3) is bounded
client-side at 30 days from `signed_at` (chosen, not measured); older, the
`publish-owed` state asks the person to sign in again, which issues nothing
new but confirms the account is still theirs before a weeks-old reset is
pushed to the directory.

#### 5. Removal from the account

- **Discovery:** the KeyPackage rows go with the attested upload
  (section 4.1).
- **DS, the DeviceGroup journal** (`services/ds`, `lakko-ds-proof`):
  `PUT /ds/group/{hex}/reset` keeps its route and its `Reset` statement
  (ADR-0057) and gains an optional second body field,
  `{statement_b64, key_reset_attest_b64}`. When the attestation is present:
  1. decode and verify it under the pinned attest key
     (`LAKKO_DS_ATTEST_KEY`, base64 verifying key; unset -> `503
     key-reset-disabled` on this arm only; the DS gains `lakko-attest` as a
     dependency and never fetches the JWKS, the ADR-0015 pinned posture);
  2. `device_group_journal_id` must equal the path's journal id, else the
     uniform `401` (a reset attestation for one journal is useless on any
     other, including a group journal the account owns: those are not
     re-rooted by this route, see phase 2);
  3. `key_generation` must be strictly greater than
     `ds_group_owner.key_generation` (new column, `DEFAULT 0`; only
     DeviceGroup journals ever hold a value above 0), else `409`;
  4. the statement is then **heard from any signer** (an amendment to
     `decide`, `ownership.rs:286-293`, where `Reset` is root-only today):
     the signer becomes the new root, `root_pk` is replaced, the epoch
     chain, the registrar set and the push binding are deleted and the
     statement's epoch key and optional registrars are written as
     `apply_reset` does (`:586-611`), and `key_generation` is recorded;
  5. the nonce is spent only after the attestation and the generation
     check pass, so a stranger learns the uniform `401` at no cost to the
     server, as ADR-0057 intends.
  Without the attestation the route behaves exactly as today. A claim or
  reset by the old root after the attested reset is a stranger's and gets
  `401`; the old devices' epoch keys are gone with the chain, so they read
  `401` on the DeviceGroup journal at once, which is what fact 17's
  "unbuilt" case in `docs/ux/second-device.md` section 3.1 becomes: it is
  now covered by section 3.5, because every such device also holds a
  superseded token.
  **ADR-0057 invariant amended:** "a claim signed by any other root is
  refused" gains one exception, a `Reset` carrying a valid key-reset
  attestation of a strictly higher generation for exactly this journal;
  and "a re-founded DeviceGroup: same root, so a root reset re-claims the
  journal" (ADR-0057 section 3) no longer describes the key-reset case,
  where the root changes with the seed.
- **push-gw:** nothing server-side (fact 9). The old device unsubscribes
  its rows at wipe (section 3.5); the attested DS reset drops the journal's
  push binding, so a wake under the old `push_gid` is dropped from then on
  even before the old device wipes; the new DeviceGroup mints a routing
  secret and binds it afresh (ADR-0042, ADR-0057).
- **The DeviceGroup's attested-removal policy** (ADR-0015) restarts off in
  the re-founded group; with every passkey deleted there is nothing to
  attest with, and `enableAttestedRemovalsIfNeeded` arms it again on the
  next passkey registration.

#### 6. UX

Specified in [`docs/ux/identity-recovery.md`](../ux/identity-recovery.md)
section 15: the warning text of "Reset account keys" now states the new
consequences (other devices signed out and removed, passkeys deleted,
contacts re-verify, old messages unreadable, a new 12-word phrase); the
signed-out screen; the Settings > Devices list after a reset.

### Phase 2 (design only; the MLS-touching part, under its own sprint and G-39)

Peers evict the old identity's leaves from the conversation groups they
share with it. Nothing in phase 1 does this; after phase 1 the old
devices still hold leaves in every group and still decrypt its traffic
(section 3.4). Phase 2 is what makes "removed from the account" true for
conversations.

- **Trigger.** A peer's client already runs the fail-closed identity
  check at every `identityFetch` (ADR-0016 `verifyBundleAccountKey`:
  TOFU only first-ever; a changed pin requires a proof, else
  `SupersessionRefused`) and reads the KT log's `is_reset` through
  `verifyIdentityInLog` (ADR-0019/0036). The trigger is: **the pin for
  `account_id` differs from the published key, there is no proof, and the
  log's entry for the new key carries `is_reset`**. The client records
  `contact.keyState = reset-unverified` with the old pinned key kept as
  `previousKey`. No automatic re-pin (invariant #6; the acceptance gate
  stays fail-closed).
- **Who proposes the removal.** In every group where the observing device's
  account is **owner or admin** (ADR-0022 role table), it proposes, through
  the ADR-0045 proposal path, the removal of every leaf whose device
  binding resolves (ADR-0050 `account_key_at_leaf`) to `previousKey`. An
  owner-or-admin may already propose any member's removal
  (`group_proposals::authorise`), so **this needs no new authorisation arm
  in `lakko-mls`**. The ADR-0050 self-service arm does not apply: the
  leaves belong to another account. A plain member does nothing and sees
  the contact flagged. Several admins proposing the same removals compose
  under ADR-0045 (the sweep deduplicates by leaf).
- **One-to-one chats (ADR-0062).** `one_to_one_holds` refuses a commit that
  removes a person, so a 1:1 with the old identity **ends**: the client
  marks it closed, keeps its history read-only, and stops sending (a send
  would reach the old devices). The UI shows "X reset their account keys.
  This chat is closed." with **Start a new chat**, enabled only after the
  re-verification step below; the new 1:1 is created with the new identity's
  devices exactly as a first chat is. The old journal is left to the DS
  prune. No `lakko-mls` change.
- **Re-verify the safety number before the new identity is admitted
  anywhere.** The contact stays `reset-unverified` until the person
  compares the new safety number out of band and confirms it in the contact
  info screen ("I verified the new safety number"). Until then: no new 1:1,
  no add to a group the person administers, and incoming messages from the
  new identity in shared groups render with the "unverified new keys"
  warning the ADR-0016 refusal already renders. Confirming re-pins the
  account id to the new key with the monotonic marker (ADR-0016's pin
  store), and from then on the new identity is an ordinary contact. The
  one-time OOB re-verification is what ADR-0017 required of a fresh
  identity; phase 2 gives it a place in the UI.
- **Groups the old identity owned.** Two problems, stated honestly:
  1. **C-63 (`owner_survives`)** refuses any commit that leaves the group
     with an owner who is not in it, and an ownership transfer needs the
     owner's key, which is gone. So the eviction above **cannot be
     committed** in a group the old identity owned: this is ADR-0022's
     open "lost owner" case made concrete. The MLS-touching proposal is a
     new owner-only-rule exception in `group_proposals`: a role-table
     rewrite that moves the owner slot to an admin is authorised when the
     proposer is an admin and the proposal carries, as evidence, the KT-log
     entry for the old owner's account key with `is_reset` and the chain
     (root signature, witness quorum, inclusion), verified by every peer at
     the receipt gate and the commit gate under the same rules a client
     applies to the log. This touches `lakko-mls` (invariant #3: ADR +
     property tests for "an admin may take the owner slot only with
     verified reset evidence; a member may not; evidence for another key
     is refused; `owner_survives` still holds after the transfer") and is
     exactly the kind of accumulated change G-39's external review must
     cover.
  2. **The DS journal root** derives from the old seed (fact 8). Admins
     keep appending epochs (`Append` at `latest + 1` is any registrar's,
     ADR-0057 section 2), so the group keeps working, but nobody can ever
     reset the chain, appoint registrars or transfer the root again. The
     key-reset attestation does **not** re-root group journals (it names
     the DeviceGroup only, section 5, deliberately: an attestation that
     re-rooted every journal the account owns would be a server-held list
     of them, invariant #5). Recommendation: a group whose owner's identity
     was reset gets a **"Recreate group"** affordance for the new owner
     (after re-verification): a new group, new journal, the members
     re-invited by the admin who took the slot, the old group closed. The
     same applies to channels, whose owner key derives from the account
     seed too (ADR-0059 `lakko/channels/owner/v1`, per CLAUDE.md; not
     re-read): a reset orphans the person's channels. Open question 6.
- **What must remain true.** Invariant #3: the only `lakko-mls` change is
  the owner-slot exception above, with its property tests; the eviction
  itself rides existing proposals. Invariant #4: a removal that cannot be
  committed (no admin online, the owned-group case) leaves the old leaves
  in place and the contact flagged, never a silent "removed" in the UI;
  the peer never re-pins without the OOB confirmation; a 1:1 ends rather
  than continuing to the old devices.

## The backup head (ADR-0037): examined, not amended

The restoring device after a reset holds the **new** phrase; a backup blob
sealed under the old phrase's KEK fails to open (`argon2id_kek(phrase,
salt)`, ADR-0064 fact 3), which the restore gate reports as an unopenable
backup, not as a rollback. The per-account `backup_version` counter is
monotone across resets and needs no generation. During the access-TTL window
the old device can re-put a blob it already holds (section 3.4): the new
device, restoring with no floor, sees a head of version `<= N` within the
30-day staleness window and opens the envelope under the new phrase, which
fails: fail-closed and visible, and the resetting device re-uploads on its
next backup cadence. A head carrying the generation would only improve the
message ("this backup predates the reset"); it is not needed for safety and
ADR-0037 is left as it is.


## Security consequences, per invariant

- **#1 (no PII).** Nothing new names a person. Auth stores one more
  integer per account (it already had the column); discovery stores a
  generation and an opaque attestation; the DS a generation per journal.
- **#2 (ciphertext and public artefacts only).** The attestation is a
  public Ed25519-signed artefact carrying no key material, like the four
  before it. The server never learns a seed, a phrase or an MLS secret in
  any step. The refresh credential is an HMAC over the account id, like
  the access token.
- **#4 (no fallback).** Every new decision point fails closed: a
  superseded token is refused, an unattested key change is refused, an
  attestation for another journal or a non-increasing generation is
  refused, a stale device wipes rather than publishes, an unverifiable
  reset leaves the contact flagged. The one place that fails **open** is
  deliberate and bounded: a bundle fetch that fails on landing skips the
  stale-keys check for that landing (as today's silent-reload mode does),
  and the device still uploads nothing.
- **#5 (no social graph on the server).** The DS verifies an artefact that
  names a per-account pseudonym (`account_ref`) and the journal it already
  serves; it does not learn the account id. The attestation re-roots one
  journal, the DeviceGroup, never the account's group journals (phase 2).
  Auth learns which DS journal id is an account's DeviceGroup, which it
  could compute from the public derivation before.
- **#6 (rotation only via a supersession proof; old keys do not come
  back).** A key reset is the one sanctioned publish-without-proof of
  ADR-0017, and it now carries an auth attestation: in ADR-0036's terms,
  the genesis attestation binds the authenticated account to its first key
  and the key-reset attestation binds it to its `g`-th, both client-asserted,
  both server-signed, neither a proof the key is "right". The acceptance
  side is unchanged and still fail-closed (ADR-0016's peer gate refuses the
  new key until the OOB re-verification). **"Old keys do not come back" is
  now enforced at the directory**: the heal path that let an old device
  republish the previous key is removed, and no unattested key change is
  accepted.
- **#7** untouched: the new tokens are wrapped under the same device key as
  today's.
- **#3** untouched in phase 1 (no `lakko-mls` change; the DS reads
  `DeviceGroup::group_id_for`'s derivation in auth, which is a hash rule,
  not the state machine). Phase 2 touches it once, named above.

Residuals, collected: a stolen session can reset (section 2, bounded,
loud); the password and TOTP survive a reset (section 2, flagged); the
access-TTL window at storage (the backup-head section) and at the discovery routes
section 4 does not gate; a refresh credential leaked from a device is good
for up to 30 days against auth only (as a session token is today), revoked
by the next reset; the old devices keep their conversation leaves until
phase 2; owned groups and channels are orphaned until phase 2's open
question is decided.

Flag for `lakko-security` before merge (auth token format and a new
anonymous route, a passkey deletion without an action grant, a new attest
kind verified by the DS, the discovery change rule), on explicit request.

## Alternatives rejected

- **Keep stateless 30-day tokens and fix only the publish-back.** The
  directory would stop flip-flopping, but every other device would keep a
  valid session for up to 30 days: it could mint call JWTs and TURN
  credentials, issue anonymous tokens, put backups, manage passkeys, and
  keep its leaves and its DeviceGroup membership. "Signed out
  automatically" would not be true of it in any sense.
- **Per-device session tokens with a server-side device registry.** Would
  give per-device revocation, which the maintainer's decision does not
  need (every other device goes, not one of them), at the cost of auth
  materialising a per-account device list, the one thing push-gw was
  deliberately built not to hold (ADR-0042). The generation revokes all
  others without naming any.
- **Variant (a) tokens** (a floor at every verifier): section 3's table.
  Storage and gif-proxy have no database to hold a floor and would need a
  feed from auth; the latency it buys over (b) is the access TTL.
- **Delete the old DeviceGroup journal instead of re-founding it.** The DS
  has no delete route; the id is deterministic, so the new founder needs
  the same id anyway; and a deleted (unclaimed) journal falls back to the
  Phase-1.3 open posture, where the old devices could re-claim it. A
  re-founding under a verified attestation is strictly better.
- **Force a password change as part of the reset.** The password is what
  the person just used; invalidating it locks out the working path to
  nothing gained (section 2, open question 1).
- **Carry `account_id` in the attestation** (the brief's layout). The DS
  would learn the account id (section 2.2).
- **Let the attestation re-root every journal the account owns.** The
  server would hold the list (invariant #5); phase 2's "Recreate group" is
  the alternative.

## Open questions

1. **The password and TOTP survive a reset.** Recorded as decided for the
   reasons in section 2; the maintainer may prefer to force a password
   change on the resetting device (the recovery phrase can set one through
   `/password/reset/*`) or to delete the TOTP secret too. Recommendation:
   keep both; a reset is already loud on every other device, and an
   attacker holding the password could reset before this ADR as well.
2. **Rename `accounts.identity_version` to `key_generation`?**
   Recommendation: keep the column name, name it `key_generation` in code
   and in the export's documentation; a rename migration can follow when
   the export's JSON is next changed.
3. **A dedicated limit scope for the reset** (for example 3 per day) beside
   `RecoveryReplace`'s 10 per hour. Recommendation: add `Scope::KeyReset =
   13`, default `Window::new(3, 86_400)` (chosen), charged on `finish`
   only, with `RecoveryReplace` still charged on both calls; a reset is
   rarer and more destructive than a credential replace was.
4. **Access TTL.** 900 s is chosen. The test stack's auth load after the
   three clients refresh in the background is the measurement that should
   set it; the knob exists for that.
5. **Should storage record a generation floor beside the backup blob** (it
   has no database, but it has a per-account file)? Recommendation: not in
   phase 1; the backup-head section's window is bounded and visible.
6. **Owned groups and channels after a reset** (phase 2): "Recreate group"
   versus the owner-slot exception with reset evidence, or both. The
   exception is the MLS-touching piece and needs the ADR, the property
   tests and the G-39 review; "Recreate group" is client work. Recommendation:
   both, the affordance first.
7. **Should the DS serve, and should discovery serve, the attestation to
   peers?** Phase 2 reads `is_reset` from the KT log, which is enough for
   the trigger; serving the attestation would let a peer show "reset on
   <date>". Not needed for either phase.

## Amendments to earlier ADRs

To be written into those files as dated notes at the top once this ADR is
accepted; their bodies are left as they are.

- **ADR-0014:** `POST /account/keys/reset/finish` deletes every passkey of
  the account without an action grant; the reasoning and the residual are
  in section 2. The per-action grant stays on `DELETE /webauthn/credentials/{id}`
  and `DELETE /account`.
- **ADR-0017:** `mintFreshIdentity` is the client half of
  `/account/keys/reset`; the publish-without-proof now carries a key-reset
  attestation; the client order is section 2.3 (server first, then
  publish); a reset signs out and removes every other device and deletes
  the passkeys.
- **ADR-0037:** not amended. The backup-head section records why the backup head does
  not need the generation.
- **ADR-0050:** section 2.10's "server-side session revocation on removal
  is arc C" is built for the reset case by section 3; the per-device
  removal from the Devices screen still revokes no session (a removed
  device's token stays valid until it expires or the next reset). The
  Devices list after a reset: UX section 15.3.
- **ADR-0053:** the DS verifies one auth-signed artefact under a pinned
  key on one route arm; everything else about proofs is unchanged.
- **ADR-0057:** section 5's invariant amendment.
- **ADR-0064:** decision 9's routes fold into the reset; "recovery
  credential owed" becomes "publish owed"; open question 7 is answered for
  the reset case (a reset revokes every other session; a plain credential
  replace no longer exists on its own).

## Sprint plan

| Sprint | Scope |
|---|---|
| S1 crates | `lakko-attest`: `KeyResetAttestation` (layout, domain, `sign`, `open`, `verify_key_reset_binding`, the freshness variant), the five-way domain-crossing test, fuzz `key_reset_attestation_open`. `lakko-auth-token`: v2 access token with `key_generation`, the `r1` refresh credential under the derived key, `verify` returning the pair, v1 refused; fuzz `token_verify` extended to both. Unit tests for every refusal. **Built 2026-10-03 as specified:** `crates/lakko-attest/src/key_reset.rs` (177 bytes, `verify_key_reset_fresh` is the freshness variant, `key_reset_account_ref` + `device_group_journal_id` derivations golden-pinned, the crossing test covers all five other kinds incl. the username certificate); `lakko-auth-token` v2 / r1 five-segment credentials, `verify` returns `VerifiedToken { account_id, key_generation }`, `blake3` added as a direct dependency for the refresh key. |
| S2 `services/auth` | `/account/keys/reset/{start,finish}` (the transaction of section 2); `/auth/session/refresh`; the generation check in `require_session`; every login route issues the pair; `/recovery/credential/*` deleted; `Scope::KeyReset` if open question 3 is accepted; the `key-generation-superseded` body. Database-backed tests (they skip without `LAKKO_AUTH_TEST_DATABASE_URL`): reset bumps and signs, superseded token refused on a bearer route and on refresh, passkeys and vaults and grants gone, the recovery credential replaced, the old phrase refused, the new phrase accepted. **Built 2026-10-03 as specified** (`src/key_reset.rs`, `src/session.rs`; `Scope::KeyReset = 13`, 3 per day, charged on finish; `LAKKO_SESSION_TOKEN_TTL_SECS` / `LAKKO_REFRESH_TOKEN_TTL_DAYS`; no migration, `identity_version` kept; every login response carries `refresh_token`, `refresh_expires_at_ms`, `key_generation` beside the two fields it had) with two deviations: the `503 key-reset-disabled` arm is not built because the attest key is loaded unconditionally at boot, and a token whose generation is ABOVE the row's (unreachable for an honestly issued one) is refused with the plain `401`, not admitted. Discovery, storage and gif-proxy verify v2 and ignore the generation (S3 adds discovery's floor). |
| S3 `services/discovery` | Migration (`key_generation`, `key_reset_attest`); the change rule of 4.1 (attested or proven, else `409`); the heal retired; the KeyPackage rows deleted on an attested change; the generation gate on the two writes; `key_generation` in the fetch; the pinned attest key (`LAKKO_DISCOVERY_ATTEST_KEY`). Tests: unattested change refused, attested change accepted once and replayed refused, lower-generation token refused, rows deleted. **Built 2026-10-03 as specified** (migration `0026`, `src/key_reset.rs`, the change rule in `identity_bundle.rs`, route tests DB-gated on `LAKKO_DISCOVERY_TEST_DATABASE_URL`), with these specifics: the upload field is `key_reset_attest` (standard base64, like `genesis_attest`; the client re-encodes auth's URL-safe value); discovery verifies the attestation (the one attestation it parses) and answers `400 key-reset-attestation-invalid` for a signature, `account_ref` or pubkey that does not bind, `409 key-generation-not-newer` for a valid attestation whose generation is not above the row's (a replay), `409 identity-change-needs-attestation` for a change carrying nothing, `429 identity-changed-too-recently` for an attested change inside the cool-down, `503 key-reset-disabled` when no key is pinned; a same-key re-upload carrying the attestation is a routine re-upload (nothing deleted, nothing moved), so a retry after a crash is safe; a token of a higher generation than the row is accepted and NOT recorded (the row's generation moves only with an attestation, else the attested upload could never be "strictly greater"); `DELETE /discovery/keypackage/device/{id}` is not under the floor (the ADR names the two writes only). A pre-existing defect found by the new tests and fixed: two concurrent first publishes of DIFFERENT accounts deadlocked on the gap lock of `SELECT ... FOR UPDATE` and surfaced as `500`; the bounded retry now covers a deadlock. |
| S4 `services/ds` | `ds_group_owner.key_generation`; the attested arm of `PUT .../reset`; `LAKKO_DS_ATTEST_KEY`; `lakko-attest` dependency. DB-gated tests: stranger with a valid attestation re-roots, old root refused afterwards, wrong journal refused, equal generation refused, no key -> `503` on that arm only. **Built 2026-10-03 as specified** (migration `0009`, `src/key_reset.rs`, the `reset` arm in `ownership.rs`, route tests over the `LAKKO_DS_TEST_DATABASE_URL` harness), with these specifics: the body field is `key_reset_attest_b64` (standard base64, like `statement_b64`) and is refused `400` on any other statement route; an attested reset on a journal nobody claimed claims it; the attested arm drops the push binding, which the plain root reset of ADR-0057 does not (that reset keeps the binding and keeps the registrar set unless one is sent); a `409` on a retry means the reset already landed at this generation, and the client confirms with the `PUT .../root` probe naming itself. |
| S5 FFI | `verifyKeyResetAttestation` on UniFFI and wasm (verify only); the refresh-credential parse is a string split the clients do themselves (no FFI). Bindings regenerated; native libraries rebuilt. **Built 2026-10-03 (bindings regenerated, native libraries and the wasm package NOT rebuilt):** `verifyKeyResetAttestation` (binding only) and `verifyKeyResetAttestationFresh` (plus a caller-supplied age bound), both taking the client's account id and deriving `account_ref` and the journal id inside, plus `keyResetAccountRef` / `deviceGroupJournalId`; UniFFI appends `FfiError::KeyResetAttestation` / `KeyResetAttestationStale`, wasm throws `invalid: ...` / `stale: ...`. A test in each FFI crate pins `device_group_journal_id` equal to `lakko_mls::DeviceGroup::group_id_for`. |
| S6 web | `SessionTokenProvider` (3.3); the reset flow in the new order with `publish-owed`; the landing rule (4.2) with the banner and heal deleted; the signed-out screen and automatic wipe (3.5); the DS attested reset in `ds/ownership.ts` and the founder; the KeyPackage binding re-derivation check (2.3 step 5); the UX of section 15. `tsc` + vitest. **Built 2026-10-03** (`apps/web/src/auth/{session-token-provider,session-401,landing-rule,key-reset-owed,wipe-account}.ts` + `AuthContext.tsx`; `tsc` + vitest 3914 + `vite build` observed by the coordinator; not walked in a browser against a server). See "What the build changed / found". |
| S7 iOS, S8 Android | The same model (the same-class rule: one pass, all three). `xcodebuild test`, `:app:testDebugUnitTest`. **Built 2026-10-03** (iOS `apps/ios/Lakko/Auth/{SessionTokenProvider,KeyReset,SessionPair}.swift`, `LakkoTests` 2613 on a simulator with the core rebuilt; Android `apps/android/.../auth/{SessionTokenProvider,SessionPair,KeyReset}.kt` + `transport/SessionBearerEngine.kt`, 2997 host unit tests with the native core not loaded and the `.so` not rebuilt); the token provider is a transport-seam interceptor on both, see "What the build changed / found". Not run on a device. |
| S9 walk | Two devices per client: reset on A, B shows the signed-out screen within the access TTL and wipes; B signs in again and restores from A; A's Devices list shows A alone; the old phrase is refused and the new one accepted; `docs/PRODUCT-READINESS.md` updated from what was seen. **Open:** nothing was walked live on any device or against a server on 2026-10-03. |
| S10 docs and security | The amendment notes in ADR-0014 / 0017 / 0050 / 0053 / 0057 / 0064; `CLAUDE.md` ADR row and module-map rows (`lakko-attest`, `lakko-auth-token`, `auth`, `discovery`, `ds`); `docs/security/invariant-index.md` (a new C-entry for "a key change at the directory is attested or proven"); `known-gaps.md` (the phase-2 gap, minted); a `lakko-security` pass on request. **Docs reconciled 2026-10-03:** the `CLAUDE.md` rows (0065 added; 0014 / 0017 / 0057 / 0064 and the five module-map rows amended), `docs/security/auth-flow.md`, `threat-model.md`, C-71 in `calls.md` + the index, G-44 (phase 2) and G-45 (the stolen-session reset) in `known-gaps.md`, `docs/PRODUCT-READINESS.md`, UX section 15. The dated notes at the top of the amended ADR files are still to be written; the `lakko-security` pass has not run. |
| Phase 2 | Its own ADR amendment or ADR, after the maintainer decides open question 6; the `lakko-mls` change with property tests; under G-39. |

## What the build changed / found (phase 1, 2026-10-03)

Every item below was read in the code on 2026-10-03 (the files named in
the References, plus the client modules listed at the end of this section)
unless marked otherwise. The sprint rows above carry the per-sprint "built
as" notes; this section collects every deviation in one place, including
the ones the client reports recorded.

**Server.**

- **No `503 key-reset-disabled` on auth** (section 2's error list): the
  attest key is loaded unconditionally at boot
  (`load_or_init_attest_key`), so the arm cannot occur and was not built.
  Discovery and the DS DO have it, on their pinned keys
  (`LAKKO_DISCOVERY_ATTEST_KEY`, `LAKKO_DS_ATTEST_KEY`; unset = the arm
  answers `503`, and on discovery that means no key can change at all
  without a supersession proof, since the heal is gone).
- **A token whose generation is ABOVE the account's row** is refused with
  the plain `401` by `require_session` and with `refresh-invalid` by the
  refresh route (unreachable for an honestly issued credential; the ADR did
  not say what to do with it). Discovery's floor, by contrast, accepts a
  higher-generation token and records nothing (section 4.1 as written).
- **`Scope::KeyReset = 13`** (open question 3, taken): 3 per day, charged
  on `finish` only; `RecoveryReplace` (10 per hour) is still charged on
  both calls. `LAKKO_AUTH_LIMIT_KEY_RESET` overrides it.
- **Response field names, as built.** `reset/start` keeps
  `recovery_request` -> `recovery_response`; `reset/finish` takes
  `recovery_upload` + `new_signature_pubkey_b64` (URL-safe base64, no
  padding) and answers `key_reset_attest_b64` (**URL-safe** base64, no
  padding, 177 bytes) plus the flattened session pair `session_token`,
  `session_expires_at_ms`, `refresh_token`, `refresh_expires_at_ms`,
  `key_generation`. Discovery's upload field is `key_reset_attest`
  (**standard** base64, the `genesis_attest` convention: the client
  re-encodes); the DS body field is `key_reset_attest_b64` (standard base64,
  the `statement_b64` convention) and is refused `400` on any statement
  route but `reset`. Every session-landing route (`/signup/finish`,
  `/login/finish`, `/recovery/login/finish`, `/password/reset/finish`,
  `/webauthn/auth/finish`, `/totp/recover`, `/device/authorize/redeem`)
  answers the same flattened pair; a password reset does NOT bump the
  generation.
- **The `/recovery/credential/{start,finish}` routes are deleted** (no
  handler, no route, no mention left in `services/auth/src`); ADR-0064
  decision 9's replace exists only inside `reset/finish`.
- **Discovery's error vocabulary** is richer than section 4.1 named: `400
  key-reset-attestation-invalid` (bad signature, another account's
  `account_ref`, a pubkey that is not the uploaded key), `409
  key-generation-not-newer` (a valid attestation at or below the row's
  generation: a replay, which a retrying client reads as "done"), `409
  identity-change-needs-attestation` (a change carrying nothing: the
  former heal lands here), `429 identity-changed-too-recently` (the kept
  cool-down), `503 key-reset-disabled`, `401 key-generation-superseded`.
  A same-key re-upload that carries the attestation is a routine re-upload
  (nothing deleted, nothing moved), so a retry after a crash is safe.
  Discovery VERIFIES the attestation (it is the one attestation discovery
  parses); the supersession proof stays presence-only, as before.
- **A pre-existing discovery defect found by the new tests and fixed:** two
  concurrent FIRST publishes of DIFFERENT accounts deadlocked on the gap
  lock a `SELECT ... FOR UPDATE` takes on an absent `identity_bundles` row
  and surfaced as `500`; the bounded retry of the upsert now treats a
  deadlock / lock-wait timeout (MariaDB 1213 / 1205) as a lost race and
  retries it (`is_lost_race`).
- **`DELETE /discovery/keypackage/device/{id}` is NOT under the generation
  floor.** The ADR names two writes (`POST /discovery/identity`, `POST
  /discovery/keypackage`) and the build gated exactly those. **Residual,
  flagged:** within the access TTL an old-generation token can still delete
  a KeyPackage row of the account, including the resetting device's
  re-published one, which the other devices would then retire from chats
  (ADR-0050 re-admission). Bounded by the access TTL and self-healing at the
  next re-publish; recorded under G-45.
- **DS:** an attested reset on a journal nobody claimed claims it; the
  attested arm drops the push binding, which the plain ADR-0057 root reset
  does not (that one keeps the binding and the registrar set unless one is
  sent); `409` on a retry means the reset already landed at this
  generation, confirmed by the `PUT .../root` probe naming the new root
  (`204`). The nonce is spent only after the attestation and the generation
  check pass, as section 5 step 5 asks.
- **FFI (S5):** `verifyKeyResetAttestation` and
  `verifyKeyResetAttestationFresh` take the client's account id and derive
  `account_ref` and the journal id inside, plus `keyResetAccountRef` /
  `deviceGroupJournalId`; UniFFI appends `FfiError::KeyResetAttestation` /
  `KeyResetAttestationStale`, wasm throws `invalid: ...` / `stale: ...`. A
  test in each FFI crate pins `device_group_journal_id` to
  `lakko_mls::DeviceGroup::group_id_for`. Bindings regenerated; the S5 row
  records the native libraries and the wasm package as not rebuilt at that
  sprint. For the test runs the iOS core was rebuilt and the Android `.so`
  was not (the Android host suite does not load it); whether the wasm
  package was rebuilt before the web run is not recorded here.

**Clients.**

- **The token provider is a transport-seam interceptor on iOS and Android,
  not a constructor value** (section 3.3 described replacing the closure
  each REST client is handed). On iOS `AntiCensorshipTransport.perform`
  hands every request carrying a bearer this session issued to
  `SessionTokenProvider.shared.perform`
  (`apps/ios/Lakko/Auth/SessionTokenProvider.swift`); on Android the Ktor
  engine wrapper `SessionBearerEngine`
  (`apps/android/.../transport/SessionBearerEngine.kt`) substitutes the
  provider's current token and applies the one-refresh-one-retry rule. The
  REST clients keep handing out the token they captured; the seam replaces
  it on the wire, so a stale capture never costs a `401`. On web the
  provider's `fetch` wrapper (`sessionFetch`) is what every authenticated
  client calls (`apps/web/src/auth/session-token-provider.ts`); the rules are
  pure functions in `session-401.ts`. Refresh-ahead is 120 s on all three;
  one refresh in flight per account.
- **The DS speaks proofs, not bearers, so a DS `401` cannot carry
  `key-generation-superseded`.** On iOS the DeviceGroup poll loop answers a
  journal `401` by asking the token provider for a refresh **at most once
  a minute** (`DeviceGroupSyncRepository.probeSupersessionIfDue`,
  `supersessionProbeIntervalMs = 60_000`): a superseded generation then
  fires the sign-out; anything else changes nothing. This is how an old
  device whose DeviceGroup epoch keys the attested reset deleted learns of
  the reset without waiting for its access token to expire.
- **The device's own KeyPackage is re-published at the next session
  landing, not as a numbered step of the reset**, on Android (the chat
  bootstrap of the landing re-publishes it with a binding re-signed by the
  new account key, `apps/android/.../auth/KeyReset.kt`). iOS keeps a
  `keyPackage` step in its owed record, but what the step does is re-mint
  the device binding under the new account key
  (`ChatRepository.installDeviceIdentity`) and hand the upload to the chat
  repository's republish (`LakkoApp` wires
  `republishKeyPackageAfterKeyReset`); web's owed record marks two steps
  only (`directory`, `deviceGroup`) and relies on its landing's KeyPackage
  publisher. The 2.3 step 5 question (does the publisher re-derive the
  binding from the session identity, or cache one signed by the old key?)
  is answered by each client re-minting the binding; it was not proven by
  a live run.
- **No DeviceGroup push routing id is bound after the re-founding** (section
  2.3 step 4 said "bind a fresh push routing id"). The natives state it in
  code: the DeviceGroup journal binds no push routing id on iOS / Android
  (only conversation groups do), so there is nothing to re-bind
  (`DeviceGroupDsAccess.refoundAfterKeyReset`); the web founding path read
  today (`apps/web/src/devices/lifecycle.ts`) binds none either. The
  attested DS reset still drops whatever binding the journal held.
- **The attestation verification posture when no attest key is pinned
  differs per client.** With a pinned key (`VITE_LAKKO_DEVICE_ATTEST_KEY` /
  `LakkoDeviceAttestKey` / `LAKKO_DEVICE_ATTEST_KEY`) all three verify
  fail-closed before using the attestation. Without one, all three fetch
  the auth JWKS and warn (the ADR-0037 F1 posture: trusting the origin that
  minted the attestation); when the fetch fails, **iOS and Android fail
  closed** (iOS throws, Android leaves the attestation unused and the
  publish owed, `PublishOwedReason.Unverified`, retried later), while **web
  skips the local check with a warning** and proceeds, on the ground that
  discovery and the DS verify under their own pinned key regardless
  (`verifyKeyResetAttestationLocally` in `AuthContext.tsx`). All three
  use the same pinned key the device-attest and backup-head verifiers use.
- **The 30-day bound (section 4.2) is implemented client-side on all three**
  as a constant (web `KEY_RESET_ATTESTATION_MAX_AGE_MS`, iOS
  `maxAttestationAgeMs`, Android `KEY_RESET_ATTESTATION_MAX_AGE_MS`, all
  `30 * 24 * 60 * 60 * 1000`) measured from the attestation's `signed_at`;
  an older owed attestation is not applied until the person signs in
  explicitly in this page / process (web `attestationGate` ->
  `recovery_phrase.fresh.sign_in_to_finish`; iOS `signInRequired`; Android
  `AttestationAge`). On iOS the fresh variant of the FFI verifier carries
  the bound; on web and Android the bound is applied beside the binding
  verifier. Neither server checks freshness, as section 2.2 says.
- **The owed-publish record** (web `key-reset-owed.ts`, iOS
  `KeyResetPublishRecord`, Android `KeyResetOwedStore`) keeps the
  attestation, its generation, the new pubkey and per-step marks in plain
  per-account storage (public, auth-signed material only; the seed stays
  under the device key), cleared when every step landed and by the wipe.
- **The signed-out screen and the automatic wipe** are built on all three
  (`devices.signed_out.*` catalog keys exist); the wipe reuses each
  client's full forget-device list (web `wipe-account.ts` with a
  `key-reset-owed` step added; iOS `wipeAfterKeyReset` ->
  `AppServices.wipeAccountLocalData`; Android the same list), publishes
  nothing first, and makes no call that needs the superseded token.
- **The landing rule** (section 4.2) is a pure function on all three (web
  `landing-rule.ts`, iOS `KeyResetLanding`, Android `LandingVerdict` in
  `IdentityPublication.kt`): matches -> land; higher published generation -> signed-out screen
  and automatic wipe; same generation but another key -> blocking screen,
  wipe on tap; 404 -> publish; fetch failure -> land without uploading. The
  `published-identity-changed` banner and the heal upload are gone.

**Not verified by this build:** anything on a live device or against a
deployed server (S9 is open); a real passkey ceremony or PRF; the
generation check under concurrent logins; the access-TTL window measured
against the test stack (open question 4).

## Consequences

- **Positive:** a reset means what the interface says: the other devices
  are signed out within minutes and cannot come back under the old keys,
  the directory cannot be reverted, the passkeys that carried the old
  phrase are gone, and the DeviceGroup belongs to the new identity. A
  stolen session that resets is loud instead of silent. Every client gets
  one rule for a `401` and one screen for "this device's keys are not the
  account's".
- **Negative:** every client refreshes a token every 15 minutes in the
  foreground; the DS verifies an auth-signed artefact (one more pinned key
  per deployment, one more dependency); the old devices keep their
  conversation leaves until phase 2, so "removed from the account" is not
  yet true of conversations; owned groups and channels are orphaned by a
  reset until phase 2; a weeks-old `publish-owed` reset asks for a sign-in.
- **Neutral:** `identity_version` finally means something; ADR-0064's
  "recovery credential owed" state is replaced by "publish owed";
  ADR-0016's supersession machinery stays dormant and its acceptance gate
  unchanged.

## References

- [ADR-0014](0014-webauthn-action-binding.md), [ADR-0015](0015-device-removal-authorization.md),
  [ADR-0016](0016-identity-supersession-transport.md), [ADR-0017](0017-phrase-seeded-identity.md),
  [ADR-0018](0018-recovery-factor-cascade.md), [ADR-0022](0022-group-admin-roles.md),
  [ADR-0036](0036-auth-attested-genesis.md), [ADR-0037](0037-backup-head-rollback-commitment.md),
  [ADR-0042](0042-push-opaque-group-routing-id.md), [ADR-0045](0045-remote-commit-epoch-fork.md),
  [ADR-0050](0050-per-device-conversation-leaves.md), [ADR-0053](0053-ds-journal-authorisation.md),
  [ADR-0054](0054-anti-abuse-without-client-work.md), [ADR-0057](0057-journal-ownership.md),
  [ADR-0062](0062-conversation-kind.md), [ADR-0064](0064-recovery-model-simplification.md)
- `crates/lakko-auth-token/src/lib.rs`, `crates/lakko-attest/src/{lib,genesis,backup_head}.rs`,
  `crates/lakko-mls/src/{device_group,device_binding}.rs`,
  `crates/lakko-ds-proof/src/statement.rs`
- `services/auth/src/{main,opaque,webauthn,webauthn_action,passkey_vault,limits}.rs`,
  `services/auth/migrations/0001_init.sql`
- `services/discovery/src/{identity_bundle,keypackage,auth,main}.rs`,
  `services/discovery/migrations/{0011,0016,0017}_*.sql`
- `services/ds/src/ownership.rs`
- `apps/web/src/auth/AuthContext.tsx`, `apps/web/src/devices/sync/removed-device.ts`,
  `l10n/catalog/en.json`
- `docs/ux/identity-recovery.md`, `docs/ux/second-device.md`
