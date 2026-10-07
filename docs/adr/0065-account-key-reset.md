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
  in "What the build changed / found". **Phase 2 built on 2026-10-04** on
  all three clients (web vitest 3996, iOS `LakkoTests` 2637, Android 3042
  on the host JVM), **without** the proposed `lakko-mls` owner-slot
  exception (maintainer's scope decision: owned groups are handled by
  Recreate group only), so `lakko-mls` is untouched and nothing new sits
  under G-39; see "Phase 2 as built". Its residual (the old identity's
  leaves stay in a group the old identity owned, in a group with no role
  table, and where no admin is the person's contact) is tracked as
  **G-44**, the stolen-session reset residual of section 2 as **G-45**. The
  maintainer's decisions of 2026-10-03 (what a reset must do) are recorded
  as given; the mechanisms are this document's and are built as written
  except where the build section says otherwise. **Phase 2 revised on
  2026-10-04 (maintainer's decision): the visible flow of "Phase 2 as
  built" is rejected and replaced by silent continuity**, where a peer
  accepts an auth-attested, KT-confirmed reset automatically and shows
  nothing; designed in "Phase 2 revised", **not built**, and the clients
  still hold the visible flow until it is. It changes G-44's remaining
  cases and amends invariant #6's acceptance side and
  [ADR-0016](0016-identity-supersession-transport.md).
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
  section 15 (added with this ADR) and section 16 (phase 2, added 2026-10-03,
  rewritten 2026-10-04 as the silent model).

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

### Phase 2 as built (2026-10-04): without the `lakko-mls` owner-slot exception

**Scope decision (maintainer, 2026-10-03):** phase 2 is built **without**
the owner-slot exception proposed above. A group the old identity owned is
handled only by **Recreate group**; no eviction is attempted there; the
exception is not designed further and `lakko-mls` is untouched (`git diff`
of `crates/lakko-mls` is empty), so phase 2 does not add to the G-39
surface. The UX is section 16 of `docs/ux/identity-recovery.md`, written
first; everything else rides the paths the design above names.

**The one new wire artefact** is `lakko-contacts::group_closed`
(`lakko/group-closed/v1\0 ‖ 0x01 ‖ closed_at_ms ‖ successor_len ‖
successor_group_id`, 32 or 64 bytes, no reason code; 17 tests, fuzz
`group_closed_decode`), on both FFI surfaces and decode-only on the NSE
surface; an admin posts it into the old group when it recreates one, and a
receiver admits it only from an owner or admin resolved at receipt under
the per-group lock, strictly newer by stamp (the group-name gate's shape).

**Built on web, iOS and Android the same day, unit-tested only, nothing
walked on a device or against a server:** web `tsc` clean + vitest 3996
(82 new) + `vite build`; iOS `LakkoTests` 2637 on a simulator with both
xcframeworks rebuilt; Android 3042 on the host JVM (the `.so` not rebuilt).
Per client: the flagged contact (`keyState`, `previousKey`, `pendingKey`,
the observation stamp and the directory's `key_generation`, persisted;
the contact record found by account id across the key change and re-keyed
on confirmation keeping rename, chat link and account id); the trigger as
designed (`proof-absent` AND the log's `is_reset`, nothing else sets the
state; the fail-closed gate still throws); the contacts-list row, the
contact-screen banner, the blocked actions, the bubble warning with the
sender resolved through the account id; re-verification over `pendingKey`
with the checkbox and the `security_critical` sentence; the silent
eviction through `removeMemberFromGroup` (the proposal path) in every
owner-or-admin role-table group, at notice time and on every chat open
while the `applied` test (on account keys, a torn roster never applied)
says no, with the ADD branch of the ADR-0050 re-admission arm suppressed
for a flagged contact and that arm now also running the identity check
per contact; the closed one-to-one (nothing sent, nothing fetched, a late
row dropped before it is stored, `Start a new chat` after verification
creating a first chat with the new keys); the owned-group notice,
`Recreate group` (the new-group flow pre-filled, the new identity excluded
until verified) posting the group-closed envelope into the old group, and
the closed-group shape with `Open the new group` once the successor is
held; the three background filters and the NSE suppress the envelope; the
orphaned-channel state on the device that reset (16.6), recorded at the
reset on Android and web and by key generation at creation on iOS, so
channels created before this build carry no mark on iOS.

**Deviations from the design and from section 16, recorded here rather
than smoothed over:**

- **The re-pin marker is `max(held marker, now in seconds)`, not the key
  generation**, on all three: ADR-0016's marker space is proof-expiry
  seconds, so a small generation would read as older than any accepted
  proof and the re-pin would be refused as backwards. The generation is
  stored on the record and on the confirmation instead.
- **Aligned in a second pass the same day** (web vitest 4008, iOS
  `LakkoTests` 2640, Android 3051; still nothing walked live): web's
  recreate sheet now owns the whole operation and stays with
  `chat.members.add.error.some_failed` + `Done` on a partial failure (a
  progress layer over the new group, since the sequential add path lives
  in the new chat's view); the closer's account key is stored beside the
  stamp and the successor on all three (web row codec v2, v1 refused), so
  `group.closed.system` names the closer after a reload and falls back to
  `chat.members.unnamed` only with no key; a closed one-to-one is inert on
  all three (Android no longer fetches its journal, drops every event of
  a batch in flight, refuses every send including call signalling, typing
  and read receipts, retires and never re-creates its push subscription,
  and suppresses its background wake; iOS stopped re-subscribing it and
  no longer dials it from the Calls tab's `Call back`); `Open contact`
  sits beside the refusal line where a surface can carry it (web: the chat
  error line and the contact list's accept failure; iOS: the `@name`
  sheet; Android: the add-contact sheet and the members-panel add field).
- **A third pass closed what the second left** (web vitest 4023, iOS
  `LakkoTests` 2649 with 0 failures logged, the run killed at its time
  limit before the final banner, Android 3066; nothing walked live). It
  also found the gate missing on paths nobody had listed: **a contact
  request sealed to a fetched or link-carried identity ran no pin check
  at all** on the bundle path of all three clients (invite accept, and on
  web and Android the people-search result), so a `/i/` link from a
  flagged contact's new identity was consumed and a request sealed to
  unverified keys. Now: the pure verdict (no pin sends as before; a
  flagged contact is refused whichever key is named; a pinned account
  named under another key goes through the identity gate, and an
  unexplained mismatch is refused) runs before anything is sealed and
  before an invite use is consumed, on web (`keys-reset-send-gate.ts`),
  iOS (`InviteAcceptGate`) and Android (`bundleRecipientVerdict`); web's
  typed `@name` now answers the 16.1 sentence too. Recreate records the
  successor at the create and a retry re-posts the close notice naming
  the same group, never creating again (durable on iOS and Android, in
  memory on web). A one-to-one call live when its chat closes is ended
  locally on iOS and Android the moment the reset is noticed (the peer's
  `End` can no longer arrive, and media was still flowing to the old
  devices); Android also skips the restart sweep for a closed one-to-one
  and suppresses its foreground wake; iOS opens the recreated group
  behind the sheet on a partial failure and its wipe now clears the
  closed-conversation, eviction and recreate stores.
- **Still open after the third pass.** (1) **Accepting an INCOMING
  contact request that arrives from a flagged contact's new keys is not
  gated** on web (the chat start is refused but the `ContactAck` is still
  sent to the key the request carried) or Android (`onAcceptClicked`
  founds the one-to-one and acks); iOS was not checked. The pending-request
  row has no line to say why, so it needs a UX decision first. (2) A
  `/c/` call-link join to an owner who is a flagged contact is not
  refused on web; on iOS and Android the join seals nothing to the
  owner's identity (the KeyPackage rides the SFU relay), so the question
  there is the roster warning, undesigned (16.10). (3) Web does not end a
  live one-to-one call when its chat closes (not examined). (4) The
  "close notice not sent" state has no catalog line: iOS and Android show
  `common.error.offline`. (5) Web's recreate retry state and its
  remaining-adds hand-off are in memory, lost on reload (as the new-group
  flow's is). (6) iOS's resume re-adds a member whose add is still a
  pending proposal and lists it as failed, and does not re-send the group
  name. (7) The network branch of each bundle gate and the recreate
  sheets' retry UI have no test: only the pure decisions do.
- **16.2 on Android is the platform's existing `SafetyNumberScreen`**,
  reached from the banner's `Verify`, not an inline section of the contact
  screen.
- **Android retries an eviction refused for a full queue on the next chat
  open**, not when the queue drains (the per-session memory + the
  pending-set check), and does not use the view model's user-facing
  `withMembershipRetry`.
- **The identity check in the re-admission arm costs one identity fetch
  per contact per open chat per ~10 min** and TOFU-pins a first-ever
  observation as a contact request would.
- **The "already recreated by {name}" guard** (16.10) is not built on any
  client: two admins recreating the same group both succeed and the later
  stamp wins the `Open the new group` target.
- **iOS's `Remove from this device`** confirms through the platform's stock
  confirmation dialog, matching the existing contact-removal and
  leave-group confirmations on that client (the no-system-UI rule applies
  to all three of those together, not to this one alone).
- **A non-contact whose stale TOFU pin exists** (web): a log-confirmed
  reset now shows the `Verify ... first.` sentence though they have no
  contact screen to verify in; the gate blocked them before too, only the
  copy changed.

**What phase 2 closes and what stays:** G-44 is closed in code for every
group where some owner or admin that is a contact of the person runs a
client (the old leaves are proposed out and the sweep lands when a
committer is online); it stays **open for a group the old identity owned**
(no eviction, only Recreate group), for a group with no role table, and
for a group where no admin is the person's contact (nobody flags, nobody
proposes). Nothing was walked live, so "closed in code" is the whole
claim. The 46 catalog keys of section 16.7 exist with machine
translations in all ten languages; the security-critical ones render in
English until reviewed.

### Phase 2 revised (2026-10-04): silent continuity

**Status: designed, not built.** The three clients hold "Phase 2 as built"
above; nothing in this section exists in code. UX:
[`docs/ux/identity-recovery.md`](../ux/identity-recovery.md) section 16
(rewritten the same day).

#### R.0 The maintainer's decision

Shown the visible flow of "Phase 2 as built" (flagged contact, banner,
mandatory safety-number comparison with a checkbox, blocked actions, bubble
warning, closed one-to-one with `Start a new chat`, owned-group notice with
`Recreate group`, closed-group shape), the maintainer rejected all of it:
"what do I care what Anna does with her keys; what matters to me is that
Anna is Anna and not someone else". Offered one quiet system row ("Anna has
new keys"), the answer was: no line of any kind, quiet or loud.

**The model.** When a contact resets their account keys, the other person
sees nothing and does nothing: the same contact, the same conversation row,
the history in place, messaging and calling available. The client does
everything silently. The only surface left is the one that predates phase 2,
the safety-number section of the contact screen, which shows the current
number.

**What still fails closed.** A key change that is not an attested reset (no
supersession proof and no valid auth-signed key-reset attestation, or the
KT log contradicts it) is refused with the `contacts.identity_changed.*`
copy at the action, as before phase 2.

**The accepted cost**, in the maintainer's terms: someone who takes over an
account (password and second factor) and resets its keys looks like its
owner until someone compares the safety number. The mitigations are that the
reset is server-attested, KT-logged, and loud for the real owner (every
device signed out, section 3.5).

**What this supersedes.** In "Phase 2" (design) and "Phase 2 as built": the
`reset-unverified` contact state and "no automatic re-pin"; the mandatory
out-of-band re-verification before the new identity is admitted anywhere;
every blocked action; the bubble warning; the one-to-one that **ends**; the
suppression of the re-admission arm's add branch; the owned-group notice and
**Recreate group** as a person's action; the closed-group shape. It keeps:
the eviction through the ADR-0045 proposal path, the `group_closed` wire
artefact (with a different receive rule, R.6), the key aliases on the
contact stores, the orphaned-channel state on the resetting device, and the
maintainer's earlier scope decision that `lakko-mls` gets **no** owner-slot
exception (`lakko-mls` stays untouched; nothing here adds to G-39).

#### R.1 Facts about the code (read on 2026-10-04, HEAD `4aa87e5d`)

1. **A peer cannot verify the attestation today.** Discovery stores it
   (`identity_bundles.key_reset_attest`, written at
   `services/discovery/src/identity_bundle.rs:720-721`) and does not serve
   it: `FetchResponse` is `signature_pubkey`, `x25519_pubkey`,
   `supersession_proof?`, `key_generation` (`identity_bundle.rs:1023-1036`),
   and the fetch selects those four columns (`:1068-1070`). The three client
   wire types match: web `apps/web/src/discovery/wire.ts:130-141`, iOS
   `apps/ios/Lakko/Discovery/DiscoveryWire.swift:210-249`, Android
   `.../discovery/Wire.kt:269-281`; `key_reset_attest` exists only on the
   upload request on each.
2. **The verifier needs no change.** `verify_key_reset_binding`
   (`crates/lakko-attest/src/key_reset.rs:343-362`) checks the signature,
   `account_ref`, the DeviceGroup journal id and the new pubkey; the FFI
   `verifyKeyResetAttestation(attestation, attestVerifyingKey,
   expectedAccountId, expectedPubkey)` derives `account_ref` and the journal
   id from the account id (`crates/lakko-ffi-uniffi/src/key_reset.rs:101-124`,
   wasm `crates/lakko-ffi-wasm/src/key_reset.rs:81-98`) and returns
   `key_generation` and `signed_at_millis`. Its only callers today are the
   resetting device's own publish (web `auth/AuthContext.tsx:1537`, iOS
   `Auth/KeyReset.swift:347/356`, Android `auth/AuthRepository.kt:2587`).
3. **The trigger as built trusts an unverified log read.** The `is_reset`
   that sets the flagged state comes from `lookupServedKeyReset`, which calls
   `fetchLatest` only: no inclusion proof, no root signature, no rollback or
   witness gate (web `transparency/verify.ts` ~`:610-643`, iOS
   `Transparency/TransparencyVerify.swift:722-748`, Android
   `transparency/TransparencyVerify.kt:830-851`). The verified path,
   `verifyIdentityInLog`, runs only after the pin gate has passed. With a
   flag and a human comparison behind it this was a hint; as the input to an
   automatic re-pin it is not enough.
4. **The KT log is not deployed and the clients run it "off".**
   `infra/systemd/` has no transparency or witness unit; `verifyIdentityInLog`
   returns `off` when no client or operator key is configured (web
   `transparency/verify.ts:226-229`).
5. **The pin store** is keyed on the peer's account id on all three; the
   monotone marker is the supersession proof's `expires_at` seconds, and
   phase 2 re-pins with `max(held, now)` (web `contacts/local-store.ts:605-607`,
   `:771`; iOS `Contacts/PeerPinStore.swift:239-249`; Android
   `contacts/PeerPinStore.kt:315-317`, `ContactKeyState.kt:146`). Web also
   stores `confirmedKeyGeneration` (`local-store.ts:199`, `:707`).
6. **A conversation is its MLS group id, everywhere.** Web: the registry
   row, the message log (`chat/message-log-store.ts:261`), the ADR-0058
   synced maps (`devices/sync/state.ts:103`), folder refs
   (`chat/folders/folder-model.ts:53-60`), pins, read watermarks, push
   routing, call log, group names, and a dozen per-group localStorage
   records. iOS and Android the same, keyed on `groupIdHex`
   (`Chat/MessageLogStore.swift:269-271`,
   `Devices/Sync/DeviceGroupSyncRepository.swift:52-71`,
   `chat/MessageLogStore.kt:271-274`,
   `devices/sync/DeviceGroupSyncRepository.kt:89-102`). **No store maps one
   group id onto another** except phase 2's navigation-only successor
   (`successorGroupIdB64` / `successorGroupIdHex`), which re-keys nothing.
7. **Key aliases exist (phase 2) and are per contact, not per
   conversation**: web `previousSignaturePubkeysB64` / `previousKeysB64` /
   `loadKeyAliases` (`contacts/local-store.ts:116-122`, `:204`, `:816`), iOS
   `ProfileDirectory.aliases` and `KeyResetEvictionStore.retiredKeys`, Android
   `ProfileDirectory.aliases`. iOS and Android drop the previous key from the
   contact on confirmation; web keeps it.
8. **A Welcome is joined without any contact check**, and creates a row
   keyed on its group id (web `contacts/MailboxContext.tsx:160-189`, iOS
   `Contacts/ContactsRepository.swift:427-454`, Android
   `contacts/ContactsRepository.kt:964-966`). The gate on unsolicited
   Welcomes is the mailbox's reach gate (ADR-0060), nothing in the client.
9. **Two one-to-ones with the same person are two rows.** No client checks
   for an existing one-to-one when creating or joining; there is no
   tie-break (web `routes/Contacts.tsx:432-453` only looks before creating).
10. **`Group::create` mints a random group id** and seeds the creator as
    owner (`crates/lakko-mls/src/group.rs:527-552`); a successor's id cannot
    be derived, so two founders cannot converge by construction.
11. **A one-to-one cannot be repaired.** `one_to_one_holds` requires every
    pre-commit account to stay and at most two accounts
    (`crates/lakko-mls/src/group_proposals.rs:724-769`); the new identity is
    a third account key.
12. **An owner's last leaf cannot be removed** (`owner_survives`,
    `group_proposals.rs:887-921`: "it would remove the group owner's last
    device"), and an ownership transfer needs the owner's key.
13. **Nobody can write to the new identity's mailbox with what they hold.**
    Reach keys derive from the account seed (`crates/lakko-contacts/src/reach.rs`,
    `reach_seed`), so a reset changes all of them and the mailbox pseudonym.
    A writer picks a kept grant by the target's account key (web
    `reach/mailbox-write.ts:99-150`); for the new key there is none, the
    write goes unsigned, and a closed mailbox answers it as stored and drops
    it (ADR-0060 section 2). So `Start a new chat` as built delivers its
    Welcome only to a person in Everyone. Not noticed before because nothing
    was walked.
14. **The re-admission arm looks up accepted contacts only**, not every
    roster account (web `chat/BackgroundReceiveContext.tsx:639-663`, iOS
    `Chat/ChatRepository.swift:5401-5407`, Android
    `chat/ChatRepository.kt:6833-6870`); this is G-44's "no admin is the
    person's contact" case.
15. **Discovery deletes every KeyPackage row at an attested change** and the
    resetting device re-publishes at its next landing (section 4.1, the
    build notes): there is a window in which the new identity has no
    KeyPackage to be added with.

#### R.2 The acceptance rule

One function per client over one FFI verifier; the decision is pure and
tested as such. Inputs: the account id; the key this device **holds** for it
(the pin, or, for a group member this device never pinned, the roster's
account key for that account id); the generation it last accepted for it;
the fetched bundle.

A fetched key that differs from the held key, with no supersession proof
(with one, ADR-0016's path runs unchanged), is **accepted as a reset** when
all of these hold:

1. the bundle carries `key_reset_attest`, and `verifyKeyResetAttestation`
   passes under the trusted attest key: the Ed25519 signature; `account_ref`
   equal to the one derived from **this** account id; the attested
   `new_signature_pubkey` equal to the fetched key. (The verifier also
   checks the DeviceGroup journal id, which is a function of the same
   account id.) No freshness bound: a reset may be weeks old when a peer
   first meets it.
2. the attested `key_generation` equals the bundle's `key_generation` and is
   **strictly above** the generation this device last accepted for the
   account. A first pin records the bundle's `key_generation` as that
   baseline (today only web stores one, and only on confirmation).
3. the KT log agrees, through the **verified** path (`verifyIdentityInLog`:
   root signature, no-rollback, the witness gate when pinned, inclusion):
   `confirmed` for the fetched key with `is_reset` true. `off` (no log
   configured in this build) passes this step: the reset then rests on the
   attest key alone, which is the posture of every build today (fact 4) and
   is stated as a residual, not hidden.

Outcomes:

| Outcome | When | What the client does |
|---|---|---|
| **accepted** | 1 to 3 hold | re-pin (below), start R.5 to R.7; no UI |
| **refused** | the attestation is absent or fails any binding; the generation is not above the accepted one; the log is `confirmed` without `is_reset`, `mismatch` or `rollback` | throw the existing `SupersessionRefused`; `contacts.identity_changed.unexplained` when the log shows no reset, `unconfirmed` otherwise; no state |
| **no verdict** | the bundle or the attest key (JWKS) could not be fetched; the log is `unreachable` or `absent` | nothing changes, nothing is shown; asked again at the next trigger |

**The attest key.** `DeviceAttestKey` as it is: the build-time pin when set,
else the cached JWKS key from auth. Without a pin the serving tier can
substitute the key, so an unpinned build trusts the serving tier for the
statement and rests on the log; with the log `off` too, it rests on the
serving tier alone. This is the same posture the device-removal and genesis
attestations already have in an unpinned build. One rule on all three
clients (the own-reset path differs today: web skips, iOS and Android fail
closed; the peer path does neither, it uses the JWKS key or has no verdict).

**The re-pin.** `pin[account id] = fetched key`, marker `max(held marker,
now in seconds)` (ADR-0016's marker space, as built), accepted generation
`g`. The held key is appended to the contact's previous keys, which are
**display aliases for stored history only** (C-68: an author is a signature
key resolved at receipt, so old bubbles are attributed through the alias);
a previous key is never accepted for new traffic, a new pin, a role or a
send. The contact row is re-keyed to the new key keeping rename, account id
and chat link (as phase 2's confirmation did). On iOS and Android the
previous key must now be **kept** on the contact (fact 7).

**Triggers.** Every existing `identityFetch` site; the re-admission arm,
extended from accepted contacts to every roster account whose account id the
device knows (contact row or the profile store, ADR-0048 v2 carries it),
which also removes G-44's "no admin is the person's contact" case; opening a
one-to-one; before a send in a one-to-one whose last check is older than the
arm's interval.

#### R.3 Server change

One, in `services/discovery`: `GET /discovery/identity/{id}` also returns
`key_reset_attest` (standard base64, omitted when null), the stored bytes
unparsed (invariant #2: discovery verified it at upload; at fetch it is an
opaque pass-through like `supersession_proof`). One column in the SELECT,
one field in `FetchResponse`, the three wire types, a route test. This
answers open question 7 ("should discovery serve the attestation to peers")
with yes, for a reason the question did not foresee.

The stored attestation is the latest reset's. After a later **proven**
rotation the published key no longer equals the attested key; that path
carries a supersession proof and never reaches the rule above.

No change to auth, the DS, the attestation layout, or either FFI verifier.

#### R.4 Reaching the new identity (new, from fact 13)

Without this nothing in R.5 to R.7 can deliver a Welcome. The new identity
knows none of its former contacts (a reset is the lost-everything path), so
it cannot hand anyone a grant; the peers hold only grants for a mailbox that
no longer exists.

**Decision: a continuity reach key.** `lakko-contacts::reach` gains

```text
continuity_reach_seed = BLAKE3-derive-key("lakko/reach/continuity/v1",
                            account_id(16) || key_generation(8, BE))
```

(a new derivation beside `reach_seed`, not a new `ReachClass` byte: it is
not derived from the account seed). The resetting device registers its
public key on the new account mailbox as part of the reset's client order
(after the attested publish, with the mailbox claim), with an expiry; a peer
that accepted the reset derives the same seed from what it verified and
signs its Welcome with `DsProofSigner.forReach(seed)`. Once the new
identity is in a conversation, its profile envelope hands out the ordinary
`contact` / `member` grants and the continuity key is not used again by that
peer. No DS change: it is one more registered reach key.

**What it admits.** Anyone who knows the account id and the generation:
former contacts and co-members (who have the id from the contact row or the
profile envelope), and the operator. Not the public: a username lookup
yields an account id only for a person in Everyone (ADR-0060), whose mailbox
is open anyway. It is weaker than a `contact` key in one way: a co-member
the person had hidden from can use it. That is ADR-0060's existing `member`
residual with a wider reach (a Welcome into any group, not only the shared
one), for the life of the key.

**Expiry.** Proposed: the DS journal retention window (30 days today), for
parity with how long an offline peer's traffic survives anyway; chosen, not
measured. A peer that returns later cannot re-establish silently; the
conversation then stays on its old session and its sends end in the ordinary
failed state. Open decision D2.

Alternatives, one line each: **carry the old mailbox's registered reach keys
onto the new mailbox at the DS** (exactly the pre-reset reach set, but it
needs an authority over the old mailbox that no longer exists, or an
unauthenticated copy route, or the old pubkey in a v2 attestation: a flag-day
change across auth, discovery, the DS and both FFI surfaces); **open the new
mailbox for a window** (admits anyone holding the public key); **a
rendezvous id derived from the account** (readable, not only writable, by
anyone who can derive it).

#### R.5 One conversation over several groups

A new MLS group has to be presented as the continuation of an old
conversation: same row, history above, pin / mute / archive / folder / draft
/ read position kept. Two shapes were possible; the choice is **a stable
conversation id with a lineage of groups**, not a re-key of the stores.

- **Conversation id** = the group id of the **first** group of the lineage
  (the root). Everything a person attaches to a conversation stays keyed on
  it and is never rewritten: drafts, flags, read cursors, conversation order
  and folder refs in the ADR-0058 synced state (`(kind, id)` keeps the root
  id, so no register is rewritten and no HLC stamp is minted for a move),
  personal pins, call history, the group name, the contact's chat link, the
  route.
- **Segments** = the lineage's groups in order. The message log stays per
  group id (fact 6); the timeline is the segments concatenated, ordered by
  `(segment index, DS message id)`, since a successor's journal ids restart.
  A reply or pin target is `(segment, message id)`; stored references of
  the old segment keep working because the old log is not touched.
- **Per-group things stay per group**: the MLS handle, the DS journal
  cursor and access keys, push routing (the successor's owner mints a new
  secret; the old group's subscription is retired), shared pins, read
  watermarks, committer beacons, pending proposals, the ADR-0040 outbox.
  "The conversation's current group" is the lineage head; sends, calls and
  the push subscription use it.
- **How a device learns the lineage.**
  (a) A device that was in the old group reads it from the **claim** the
  founder posts there (R.6): the `group_closed` envelope naming the
  successor. This is every member's device, for a group, and every other
  device of the same person, for a one-to-one.
  (b) A device added later, and a restore, cannot read the old group. The
  ADR-0058 synced state gains a write-once **lineage register**
  (`successor group id -> root conversation id`), carried in `StateSnapshot`
  and the backup payload like the other maps (`lakko-sync`, both FFI
  surfaces). Without it, such a device shows the conversation under the
  successor's id and the person's folder / pin / mute for it do not apply
  there.
  (c) The new identity's own device has no old group: for it the successor
  is the root.
- **The old segment after the move** is read-only local history. Its journal
  is still polled for the journal retention window, and rows from senders
  that are **not** the superseded identity are stored and shown (a member
  who has not moved yet, R.7); rows from the superseded identity's leaves are
  dropped before storage. A one-to-one's old journal is not polled at all
  after the move (its only other sender is the superseded identity).
- **The contact** resolves a chat by the conversation id, and a one-to-one's
  peer by account id (through the aliases), so a second one-to-one group with
  the same account never becomes a second contact or a second row once it is
  in the lineage.

Rejected: **re-key every store from the old group id to the new one at the
switch** (rewrites the synced registers on every device of every member,
each minting stamps for the same move; message ids collide in one log; a
crash mid-way leaves a conversation split across two ids).

#### R.6 Founding a successor: claim, then build

Used by the one-to-one (R.7 case 1) and the owned group (case 3). The goal
is **one** successor although several devices may act at once and group ids
are random (fact 10).

1. A device that accepted the reset and is **eligible** (below) creates the
   successor group locally (`Group::create`, itself alone) and claims its DS
   journal. Nothing is sent to anyone yet.
2. It posts a `group_closed` envelope naming the successor into the **old**
   group and notes the DS message id the publish returned.
3. It reads the old journal up to that id. If an **earlier** eligible claim
   is there, it lost: it discards its local successor (never announced, no
   Welcome sent; its empty claimed journal is left to the DS prune) and
   adopts the earlier one. Otherwise it won.
4. The winner builds: adds the members (R.7), sends the Welcomes, re-sends
   the group name with its original stamp (ADR-0051), restores roles.

**Receive rule for `group_closed`, replacing the built one** ("owner or
admin, strictly newer stamp wins"). The codec is unchanged
(`lakko/group-closed/v1`). A receiver honours a claim only when: (i) it has
itself accepted a reset (R.2) of the account that makes this group
unrepairable, i.e. the other participant of a one-to-one or the owner of a
group; (ii) the sender, resolved at receipt (C-68), is eligible; (iii) it is
the **first** such claim in the journal's order. Later claims are ignored
for good; a claim that arrives before the receiver has its own verdict is
kept and re-judged after the check the claim itself triggers. A claim from
the superseded identity's leaves is never eligible, which matters: a removed
device that still holds the old keys can post into the old group, and
"first in the journal wins" without (ii) would hand it the conversation.
Journal order is the DS's arrival order, which every member reads
identically; the per-group lock covers the read-decide-write (C-62
sub-invariant 5).

**Eligible.** One-to-one: any device of the surviving account. Group: an
admin in the live role table other than the superseded owner; when the table
names no other admin, any member that is not the superseded owner. No rank
and no liveness order is needed for correctness (step 3 decides); to avoid
wasted claims a device re-reads the journal head before step 1 and stands
down if an eligible claim is already there.

Since `Recreate group` is gone as an action, a `group_closed` envelope has
no other producer; a claim that fails (i) is dropped as it would be from a
plain member today.

Alternative, one line: a deterministic founder (lowest committer rank,
ADR-0045's order) needs a liveness oracle to move past an offline founder
and still races; claim-then-build needs neither.

#### R.7 The four cases

**1. One-to-one.** Not repairable (fact 11). The surviving side founds a
successor one-to-one by R.6 (the new identity cannot: it knows nobody). The
founding commit adds the new identity's devices and the founder's own other
devices in one commit (ADR-0050). Welcomes: to the new identity's account
mailbox under the continuity key (R.4); to the own account mailbox as today.
The founder then sends, in the successor, the messages of its own account
that were sent in the old one-to-one after the attestation's `signed_at`
(they reached only removed devices). Until the successor exists a send is
**held** (never sent into the old group). The new identity's device joins
the Welcome as it joins any (fact 8) and sees a first chat.
If the new identity sends a contact request first, the receiver runs R.2 on
the sender's account id (the request's profile carries it), and on
**accepted** answers through this same path using the request's `reply`
grant for the Welcome and the ack; no request row is shown. On **refused**
nothing is shown and nothing is sent.

**2. A group with a role table where the old identity is a member or an
admin.** Repairable in place. Every owner or admin that accepted the reset
proposes, through the ADR-0045 path: `Remove` for every leaf whose device
binding resolves to the previous key, and `Add` for every published device
of the new identity. The committer writes the Welcome under the continuity
key, which every candidate derives for itself, so no committer delegation
(ADR-0060 section 6) is needed for this add. Several proposers compose in
one sweep. The re-admission arm's
add branch is **no longer suppressed**: for an account whose reset was
accepted it now proposes the new devices, and its lookup uses the re-pinned
key. A plain member proposes nothing (the core refuses it: the new account
key is not in the group, so ADR-0050's self-service arm does not apply) and
needs to do nothing.
If the previous key was an admin, the **owner's** device proposes the role
table with the new key in its place (owner-only, one role change per sweep,
C-65); this also retires the ghost admin of the first design. Until an
owner or admin is online nothing changes and nothing is shown.
The conversation id and the group do not change. Clients suppress the
`Members changed` row for a commit whose set of **people** (by account id
through the aliases) is unchanged.

**3. A group the old identity owned.** Not repairable in place (fact 12) and
its journal root is gone (fact 8 of the Context). A successor is founded by
R.6. The winner adds every account of the old roster except the superseded
key's leaves, resolved by account id (contact row or profile store), using
the Welcome grant it holds for each (`contact`, else the `member` key scoped
to the **old** group id, which the lineage makes the same conversation), and
the new identity's devices under the continuity key. It then restores the
admin set (one role change) and, once the new identity is a member, proposes
the **ownership transfer** to it (`proposeOwnershipTransfer`, the outgoing
founder staying an admin if it was one), with ADR-0057's root handover as
for any transfer. So the group ends with the same people, the same roles and
the same owner, on keys the removed devices do not hold.
A member's device switches its sends to the successor when it has **joined**
it; before that it keeps sending in the old group, and moved members keep
reading those rows (R.5). Any moved admin or owner re-checks the old roster
against the successor's at the re-admission arm's cadence and adds whoever is
missing.

**4. A group with no role table.** `Group::create` has seeded one since
ADR-0022 and there is no deployment, so no conversation group should lack
one (`PublisherGroup` and `DeviceGroup` are not conversations). If one is
met, nothing is done: the old leaves stay. Not verified that none exists in
any account's local state.

**Channels.** Unchanged: the resetting device's orphaned-channel state
stays; a co-publisher or subscriber who reset is not re-admitted by anyone
(a subscriber re-subscribes with its new key as a new subscriber).

#### R.8 The person's own devices, and ordering

- **No KeyPackage yet** (fact 15). A founder or proposer that finds no
  published device for the accepted key does nothing and retries at the next
  trigger; a held one-to-one send waits. The reset itself is already
  accepted and re-pinned.
- **A second device of the new identity** (paired later) is admitted by the
  ordinary ADR-0050 arms into every successor and repaired group: its
  account key is now a member.
- **Two devices of the same peer** act at once: proposals compose (case 2);
  claims resolve by R.6 (cases 1 and 3).
- **A second reset before the first was handled.** The rule accepts the
  highest generation; previous keys accumulate as aliases; a successor
  founded for generation `g` whose members are superseded at `g+1` is handled
  again, and the lineage simply grows.
- **A removed device inside the access-TTL window** can still read the old
  groups (section 3.4) and can post there; R.6 (ii) and the drop of its rows
  are what bound that.
- **A peer that never comes online** keeps the old leaves in its view; the
  others are unaffected.

#### R.9 Security consequences

- **Invariant #6 and ADR-0016 gain an exception, stated plainly.** ADR-0016's
  rule is "changed pin + absent or invalid proof = refuse, no blind TOFU".
  From this section on: a changed pin with no supersession proof is accepted
  **when it is an auth-attested, KT-confirmed key reset at a higher
  generation**. A supersession proof is a proof of **key continuity** (the
  old key signed the new one). A key-reset attestation is a proof of
  **account continuity** only: the server states that whoever authenticated
  as this account asked for this key. It says nothing about who that was.
  Invariant #6's wording ("identity rotation only via a KT log supersession
  proof") no longer describes the acceptance side and has to be amended by
  the maintainer; proposed text: "Identity rotation only via a supersession
  proof, or via a server-attested key reset recorded in the KT log. Old keys
  do not come back." The second sentence is unaffected (section 4.1).
- **What the operator can now do.** Whoever holds the auth attest signing
  key, or whoever can authenticate as the account (password and second
  factor, or a stolen session: G-45), can replace a contact's identity
  toward every peer **silently**, where before the peer was stopped and
  asked to compare. A peer that never compared a safety number has no
  defence of its own.
- **What bounds it.** (1) The KT log: the substitution has to be logged as a
  reset for the account, where the owner's own devices and any monitor can
  see it; with a pinned witness quorum (ADR-0039) the operator cannot show
  one log to the victim's peers and another to the owner. This bound is
  **absent today**: the log is not deployed and clients run it `off` (fact
  4), so until it is, the bound is the attest key alone. (2) The real owner's
  devices are signed out by the generation bump (section 3), with a screen
  that says the keys were reset elsewhere: a takeover is loud for the
  victim even though it is silent for the peers. (3) The safety number
  changes, for anyone who compares. (4) The generation is strictly
  increasing per account on every peer, so an old attestation cannot be
  replayed to roll a contact back.
- **#2** unchanged: the attestation is a public signed artefact; the
  continuity seed is derived from public values and is a write permission,
  not a secret that protects content.
- **#3** untouched: no `lakko-mls` change. `lakko-contacts::reach` and
  `lakko-sync` change.
- **#4.** Every negative verdict still refuses; "no verdict" changes
  nothing. A send is never redirected to an old session silently: it is held
  for the successor (case 1) or stays in the group the device is in (case
  3), and the old leaves read a group only until the repair lands. What is
  new is that the person is not told a repair is pending.
- **#5.** Discovery serves, to anyone who asks by account id, the fact that
  an account reset and when (`signed_at`) and at which generation. The
  generation was already served; the date is new. The continuity key is one
  more public key on a mailbox; the DS does not learn why.
- **ADR-0060.** The continuity key lets co-members the person hid from
  deliver a Welcome for its lifetime (R.4), and the new identity's device
  joins what it is added to (fact 8).

**G-44's remaining cases change.** "A group the old identity owned" is
handled by the successor (case 3) instead of being left; "no admin is the
person's contact" is removed by the wider lookup (R.2); "a group with no
role table" stays. New residuals that take their place: the window before a
repair lands (as before); a member that cannot be moved to a successor; a
peer beyond the continuity key's expiry; all with the log `off`. The
coordinator reconciles `known-gaps.md` and the invariant index after the
build.

#### R.10 Decisions left to the maintainer

- **D1. The new identity's side.** As designed, its device joins a
  re-established chat without asking (fact 8, today's behaviour for any
  Welcome). Alternative: such a Welcome lands as a contact request and the
  peer's messages wait until it is accepted. Chosen: join, because the
  maintainer's rule is that nothing is asked; it is their call whether that
  rule also covers the person who reset, for whom every chat is arriving
  from someone the device cannot recognise.
- **D2. The continuity key**: its existence (R.4), its gate ("knows the
  account id"), and its expiry.
- **D3. The log `off` posture** (R.2 step 3): accept on the attestation
  alone until the log is deployed, or refuse every reset until it is.
  Chosen: accept; refusing would make the feature untestable on the only
  stack that exists and would show the refusal copy for every reset.
- **D4. Invariant #6's text** (R.9).
- **D5. The re-send of messages sent after `signed_at`** (case 1): without
  it they are lost with no sign; with it the founder re-sends content the
  person did not ask to re-send.

#### R.11 What has to be removed (per client)

From "Phase 2 as built". Files that exist only for the visible flow are
deleted with their tests; hunks in shared files are removed or changed to
the silent path.

**Web** (`apps/web/src/`). Delete: `chat/keys-reset.ts`,
`chat/conversation-closed.ts`, `chat/RecreateGroupSheet.tsx`,
`chat/recreate-group.ts`, `chat/recreate-progress.ts`,
`contacts/keys-reset-send-gate.ts`, `contacts/keys-reset-open-contact.ts`
and their tests. Rework: `chat/group-closed-store.ts`,
`chat/group-closed-envelope.ts`, `chat/group-closed-idb.ts`,
`chat/GroupClosedContext.tsx` (become the claim store and the R.6 receive
rule). Hunks: `contacts/local-store.ts` (`KeyResetRecord`, `keyState`,
`recordKeyReset`, `confirmKeyReset`, `loadFlaggedContacts`; keep the
aliases and the generation), `contacts/client.ts:579-613`, `:1670-1686`,
`:1717-1830` (trigger and the unverified gates become R.2),
`contacts/ContactListPane.tsx:270-288`, `contacts/ContactDetailView.tsx`
(`:201-236`, `:266-292`, `:349-378`, `:410-513`), `routes/Contacts.tsx`,
`routes/NewGroup.tsx:65-72`, `:204-207`, `routes/Invite.tsx`,
`search/*`, `contacts/errors.ts`, `contacts/supersession-message.ts:49-70`,
`routes/Chat.tsx` (`:3084-3573` and the hunks the recon listed at `:2246`,
`:4104-4113`, `:4173-4179`, `:4256-4259`, `:5974-5980`, `:7161-7224`,
`:7467-7477`, `:7727-7778`, `:8249-8288`, `:8445-8469`, `:8683-8765`),
`chat/TimelineBubble.tsx` (warning and `keys-reset-removed` row),
`chat/BackgroundReceiveContext.tsx:441-551` (`evictResetFor`: keep the
eviction, drop the close) and `:645-662` (drop the add suppression),
`chat/conversation-rows.ts:187-216`, `styles.css` (`.lk-keys-reset-banner`,
`.lk-sender-warning`), `auth/wipe-account.ts`. Keep: `channels/orphaned-by-reset.ts`
and its screens; the service worker's group-closed arm.

**iOS** (`apps/ios/Lakko/`). Delete: `UI/Chat/RecreateGroupScreen.swift`;
most of `Contacts/ContactKeyReset.swift` and `Chat/ConversationClosed.swift`
(keep what R.6 needs of `GroupClosedGate` and the retired-keys map) and the
matching tests in `LakkoTests/ContactKeyResetTests.swift`. Hunks:
`Chat/ChatRepository.swift:1443-1865` (facts provider, `noteContactKeyReset`,
`closeOneToOne`, `closeGroup`, `recreateGroup`; keep the eviction proposals)
and the closed gates at `:2346`, `:3695`, `:3951`, `:4103-4120`, `:4529`,
`:5390`, `:5409-5449` (identity check becomes R.2, suppression removed),
`Chat/DeviceReadmission.swift:73-96`, `Contacts/ContactsClient.swift:147-150`,
`Contacts/ContactsRepository.swift:744-798`, `:1146-1163`,
`UI/Contacts/ContactDetailScreen.swift` (banner, verify-first, the reset
shape of `SafetyNumberScreen`), `UI/Contacts/ContactsScreen.swift:298-311`,
`ContactsViewModel.swift`, `AddContactSheet.swift`, `ContactPickerScreen.swift`,
`InviteLandingScreen.swift`, `UI/Chat/ChatListScreen.swift:280-285`,
`:1040-1055`, `UI/Chat/ChatScreen.swift` (bubble warning, terminal row,
closed composer bar, members-sheet notice and Recreate),
`UI/Chat/ChatViewModel.swift:1467-1705`, `NewGroupScreen.swift`,
`Calls/CallHost.swift:223-225`, `:439` (keep ending a live one-to-one call
when its conversation moves). Keep: `Channels/ChannelsUiState.swift`
orphaned state, the NSE group-closed arm.

**Android** (`apps/android/app/src/main/kotlin/me/lakko/app/`). Delete:
`ui/chat/RecreateGroupDialog.kt`; most of `contacts/ContactKeyState.kt`,
`chat/KeysReset.kt`, `chat/GroupClosed.kt` (keep the envelope sniff and what
R.6 needs) and tests `KeysResetTest`, `GroupClosedTest`,
`ContactKeyStateTest`, `ClosedOneToOnePushWakeTest`. Hunks:
`chat/ChatRepository.kt:5114-5528` (keep the eviction), the send gates at
`:3491-4031`, `:6044-6056`, `:6834-6869` and `:6889-6894` (suppression
removed), `contacts/ContactsRepository.kt:146-354`, `:1373-1394`,
`contacts/ContactsClient.kt:303-357`, `:468-519`,
`ui/contacts/ContactDetailScreen.kt` (banner, blocked actions,
`SafetyNumberScreen` reset shape), `ContactsScreen.kt:446-473`,
`ContactsViewModel.kt`, `ui/chat/ChatScreen.kt` (bubble warning, terminal
row, `ClosedComposerBar`, members-sheet notice), `ChatViewModel.kt:353-498`,
`ChatListScreen.kt:882-947`, `NewGroupScreen.kt`, `push/*` closed
one-to-one suppression, `calls/OneToOneCallHost.kt` (keep the end-on-move).
Keep: `channels/OrphanedChannels.kt`, the group-closed arm of
`PushServiceEnvelopeFilter.kt`.

**Catalog.** 43 keys of the 46 plus `contacts.identity_changed.reset` and
`reset_unnamed` are deleted from `l10n/catalog/*.json` and the embedded
copies; `channels.keys_reset.*` stays;
`restore_identity.reset.effect_contacts` and the new-phrase sentence change
(UX 16.9).

#### R.12 The contract the client sprints build against

- Wire: `IdentityFetchResponse.key_reset_attest?: base64` (standard).
- FFI (existing): `verifyKeyResetAttestation(att, attestKey, accountId,
  fetchedKey) -> { keyGeneration, signedAtMillis }`.
- FFI (new): `continuityReachSeed(accountId, keyGeneration) -> 32 bytes`;
  the lineage register on `SyncedState` (set, read, snapshot, merge).
- Pure decision, one per client, same table: `resetVerdict(heldKey,
  acceptedGeneration, bundle, attestCheck, logResult) -> accepted{g,
  signedAtMs} | refused{unexplained | unconfirmed} | noVerdict`.
- Pin store: `acceptedKeyGeneration` per account (baseline = the bundle's
  generation at first pin); previous keys kept as aliases on all three.
- Conversation model: `conversationId` (root) and `segments[]`;
  `currentGroup(conversationId)`; timeline order `(segment, messageId)`;
  every person-facing store keyed on `conversationId`, every protocol store
  on the group id.
- `group_closed` receive rule of R.6 (first eligible claim in journal order,
  receiver has its own accepted reset, never from a superseded key), under
  the per-group lock.
- Claim-then-build with the discard on a lost claim; nothing sent before the
  claim is confirmed.
- One-to-one: held send until the successor exists; re-send of own messages
  after `signedAtMs`; old journal not polled after the move.
- Owned group: send where the device is; old journal polled for the
  retention window with superseded senders dropped; the completeness
  re-check.
- Role-table group: `Remove` old leaves + `Add` new devices as proposals;
  owner restores an admin; re-admission lookup over every roster account
  with a known id; no add suppression.
- Welcome to the new identity signed with the continuity key; the resetting
  device registers it with the expiry in its reset order.
- `Members changed` suppressed when the set of people is unchanged.
- An incoming contact request or `/i/` link from an account that is already
  a contact: R.2, then answer silently or drop; never a request row.
- A live one-to-one call ends when its conversation moves (as built on iOS
  and Android; web to match).
- No UI, no catalog key, no notification for any of it.

#### R.13 Sprint plan

| Sprint | Scope |
|---|---|
| R-S1 core and server | Discovery: serve `key_reset_attest` on the identity fetch, route test. `lakko-contacts::reach`: `continuity_reach_seed` with a golden and a domain-separation test against `reach_seed`; both FFI surfaces. `lakko-sync`: the lineage register (fold, snapshot, backup payload), proptests like the other maps; both FFI surfaces. Bindings regenerated, native libraries and the wasm package rebuilt. No `lakko-mls` change. |
| R-S2 web (reference) | Remove the visible flow (R.11) and the catalog keys. `resetVerdict` and the re-pin; the wider re-admission lookup; the conversation lineage (id, segments, timeline order, stores); claim-then-build and the new `group_closed` rule; the four cases; the continuity key on both sides (register at reset, sign on Welcome); held send and re-send; the contact-request arm. `tsc`, vitest on every pure decision, `vite build`. |
| R-S3 iOS, R-S4 Android | The same model, one pass each (the same-class rule), including the three background readers treating a successor as its conversation. `xcodebuild test`, `:app:testDebugUnitTest`. |
| R-S5 walk | Two accounts, two devices each plus a reset: one-to-one continues in the same row and a message goes through; a role-table group swaps the devices with no row; an owned group continues in the same row with the owner restored; a non-attested change is refused with the kept copy. `docs/PRODUCT-READINESS.md` from what was seen. `lakko-security` on explicit request: this changes an acceptance gate (invariant #6). |

#### R.14 Not verified, and open on paper

- Nothing here was built or run. The recon of the three clients was done by
  reading; the line numbers are HEAD `4aa87e5d`.
- Whether the resetting device today claims its new account mailbox and
  registers its reach keys right after a reset was not read; the continuity
  registration assumes that step exists to extend.
- Whether a receiving client can tell which reach key a mailbox write was
  signed with was not read; the design does not depend on it.
- Whether each client's send path has a queue that survives a restart (the
  held send, the re-send) was not read.
- Whether the profile store exposes account id by account key on all three
  for non-contacts (the wider lookup, the successor's member resolution) was
  read for web only in outline.
- A member that cannot be resolved or reached when an owned group's
  successor is built keeps writing in the old group and does not see replies
  until the re-check adds it. No bound on that is designed.
- An admin that is offline when the claim lands, comes back, and had created
  nothing is unaffected; an admin that had built a successor and lost the
  claim **after** sending Welcomes cannot occur under R.6's order, and that
  order is the only thing preventing it.
- The owner restore in case 3 gives a group to whoever reset the owner's
  account, with no member asked. That is the accepted cost applied to
  ownership.
- The date of a reset becomes public to anyone who can fetch the bundle
  (R.9, #5).

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
  new key until the OOB re-verification). **Superseded for phase 2 by
  "Phase 2 revised" R.9 (2026-10-04): a peer accepts an attested,
  KT-confirmed reset automatically; the consequence is stated there.** **"Old keys do not come back" is
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
by the next reset; the old devices keep their conversation leaves until a
contact who administers the group evicts them (phase 2, built 2026-10-04 in
code), and keep them for good in a group the old identity owned, which is
recreated instead; owned channels are orphaned (open question 6, decided).

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
   both, the affordance first. **Decided 2026-10-03 (maintainer): Recreate
   group only; the exception is not built.** Channels: the owner's own
   device shows the orphaned state and "Remove from this device";
   subscribers get no signal (UX 16.6).
7. **Should the DS serve, and should discovery serve, the attestation to
   peers?** Phase 2 reads `is_reset` from the KT log, which is enough for
   the trigger; serving the attestation would let a peer show "reset on
   <date>". Not needed for either phase. **Answered 2026-10-04: discovery
   serves it, because the revised phase 2 has the peer verify the
   attestation itself ("Phase 2 revised" R.3).**

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
- **ADR-0016 (added 2026-10-04, "Phase 2 revised"):** the fail-closed rule
  "changed pin + absent or invalid proof = refuse" gains one exception: an
  auth-attested, KT-confirmed key reset at a strictly higher generation is
  accepted and re-pinned automatically (R.2), as a proof of account
  continuity, not of key continuity. A dated note is at the top of that
  file.
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
| Phase 2 | **Built 2026-10-04** on web, iOS and Android without the `lakko-mls` owner-slot exception (open question 6 decided: Recreate group only); the `group-closed` envelope in `lakko-contacts` + both FFI surfaces + the NSE decode arm; UX section 16; the 46 catalog keys with machine translations. See "Phase 2 as built". |

## Amended 2026-10-04: password-fresh reset grant

Maintainer decision, 2026-10-04; closes the main part of G-45. Built the
same day in `crates/lakko-auth-token` and `services/auth`; the three
clients are not wired yet, so until they are every client-side reset is
refused `403`.

**The problem.** `POST /account/keys/reset/{start,finish}` needed a bearer
session and nothing else. A stolen session token, or a device left
unlocked, could therefore reset the account keys: sign out and wipe every
other device, delete the passkeys, and become the account toward every
contact.

**The rule.** A reset requires the account password proven just now, not
merely a session.

**The reset grant.** A third credential of `lakko-auth-token`, beside the
access token and the refresh credential:

```text
k1.<account_id_b64url>.<key_generation>.<expires_at_ms>.<tag_b64url>
```

The tag is HMAC-SHA256 under `BLAKE3-derive-key("lakko/key-reset-grant/v1",
session secret)`, a key of its own, so a grant is refused as a bearer and at
the refresh route by its version marker and could not be mistaken for either
even without that check, and neither of those is accepted as a grant
(`TokenSecret::issue_reset_grant` / `verify_reset_grant`). No database row,
no migration.

- **Minted by** `POST /login/finish` (the password OPAQUE login) only. Its
  answer gains `reset_grant` (string, opaque to the client) and
  `reset_grant_expires_at_ms` (unix milliseconds), flattened beside the
  session pair. No other session-landing route returns one:
  `/recovery/login/finish` (whoever holds the phrase holds the keys and has
  nothing to reset), `/webauthn/auth/finish`, `/totp/recover`,
  `/device/authorize/redeem`, `/password/reset/finish`, `/signup/finish`,
  `/auth/session/refresh`, and `/account/keys/reset/finish` itself. The
  grant is a type of its own (`session::ResetGrant`), not a field of the
  shared `SessionPair`, and the handler the password and recovery logins
  share does not mint it; a unit test pins the keys of `SessionPair`.
- **Lifetime:** `LAKKO_KEY_RESET_GRANT_TTL_SECS`, default 600. Chosen, not
  measured.
- **Required by** both reset calls, as `reset_grant` in the JSON body. A
  grant that is missing, malformed, expired, tagged by anything but this
  server, issued for another account than the bearer's, or issued at a key
  generation other than the account's current one is answered
  `403 {"error":"password-required"}`: one code, nothing done, nothing
  changed. The grant is checked first in both handlers, before the body
  (`400`) and before any limiter (`429`); a request without a bearer is
  still the `401` of the session middleware.
- **Single use, without state.** The grant names the key generation it was
  issued under. `finish` compares it with `accounts.identity_version` under
  `SELECT ... FOR UPDATE` in the transaction that then bumps it, and requires
  them EQUAL. The first reset therefore spends every grant of that
  generation, a racing second request included, and a second reset needs a
  second password login (the pair the reset returns carries no grant).
  `start` writes nothing and may be repeated while the grant lives; one
  grant serves both calls of one reset.
- **Limits.** A refused grant charges neither `Scope::RecoveryReplace` nor
  `Scope::KeyReset`, so a stolen session hammering the routes cannot spend
  the owner's three resets a day. It costs the server one HMAC and one
  indexed read per call.
  The one charged refusal is a request whose grant passed the check and
  then lost the row lock to a concurrent reset of the same account.
- **Not required:** a TOTP code (the authenticator usually lives on the
  phone that was lost; requiring it would lock a lost-everything person
  out) and an ADR-0014 passkey grant (unchanged: the passkey may be gone
  too).

**What it changes in section 2.** `start {reset_grant, recovery_request}`,
`finish {reset_grant, recovery_upload, new_signature_pubkey_b64}`; step 1 of
the transaction is now "lock the row, require the grant's generation, bump".
Everything else in section 2 stands.

**What the clients owe (unbuilt).** Keep the grant in memory beside the
password login that returned it, never in durable storage; send it in both
reset calls; on `403 password-required`, or when
`reset_grant_expires_at_ms` has passed, ask for the password and run
`/login/{start,finish}` again. A person who landed by passkey, TOTP,
another device or a refreshed session is asked for the password before a
reset.

**Residual.** Someone holding a session AND the password can still reset;
that is the account's owner as far as the server can tell. A grant lifted
from a device's memory together with the session within its lifetime is as
good as the password for one reset. Password plus TOTP survive a reset as
before.

**Tests** (`services/auth/src/opaque.rs`, gated on
`LAKKO_AUTH_TEST_DATABASE_URL`): no grant and ten unusable grants refused
on both calls with the generation, the passkey rows, the recovery
credential and the session intact, and the owner's single budgeted reset
still succeeding afterwards; a grant from before a reset, and its sibling,
refused after it; two finishes racing on one generation yield one reset;
only the password login's answer carries a grant, and a recovery-login
session cannot reset. The earlier key-reset tests log in by password first.
`crates/lakko-auth-token`: round trip, the six cross-kind directions, every
refusal; the `token_verify` fuzz target drives the grant verifier too.

## Amended 2026-10-06: fresh-auth grant for delete and add-device

Maintainer decisions, 2026-10-06. Built the same day in
`crates/lakko-auth-token` and `services/auth` (unit and DB-gated route
tests; nothing walked live); the three clients are not wired yet, so until
they are every client-side reset, account deletion and device add is refused
`403`. The section above is left as written; where the two differ, this one
holds.

**One rule for three actions.** Reset account keys, delete the account, and
add a device each require the person to have proven who they are within the
last ten minutes, by PASSWORD or by PASSKEY. The server enforces it.

**The grant is renamed and generalised** (flag day, no dual accept: nothing
is deployed). The `k1` credential of 2026-10-04 is now the **fresh-auth
grant**:

- wire format unchanged (`k1.<account_id>.<key_generation>.<expires_at_ms>.<tag>`);
  the tag key is `BLAKE3-derive-key("lakko/fresh-auth-grant/v1", session
  secret)` (renamed from `lakko/key-reset-grant/v1`: the grant is no longer
  about the reset alone);
- code: `TokenSecret::issue_fresh_grant` / `verify_fresh_grant`,
  `session::FreshGrant`, `session::require_fresh_grant`;
- response fields `fresh_grant` + `fresh_grant_expires_at_ms`; request field
  `fresh_grant`;
- lifetime `LAKKO_FRESH_GRANT_TTL_SECS`, default 600 (chosen, not measured);
  `LAKKO_KEY_RESET_GRANT_TTL_SECS` is gone;
- the refusal is `403 {"error":"fresh-auth-required"}` (was
  `password-required`: a passkey now satisfies it too, and no client was
  wired to the old code).

**Minted by** `POST /login/finish` (password) and `POST /webauthn/auth/finish`
(passkey, both the allow-list and the discoverable flow), at the generation
of the session pair in the same answer. No other route: not
`/recovery/login/finish`, `/totp/recover`, `/device/authorize/redeem`,
`/password/reset/finish`, `/signup/finish`, `/auth/session/refresh`, nor
`/account/keys/reset/finish`. The shared `SessionPair` still cannot carry
it (the unit test pinning its keys stands).

**Required by:**

| Action | Route | Where the grant rides | Spent? |
|---|---|---|---|
| Reset account keys | `POST /account/keys/reset/start`, `/finish` | JSON body `fresh_grant` | Yes, by `finish` (it bumps the generation the grant names) |
| Delete account | `DELETE /account` | header `x-lakko-fresh-grant` | No (the account is gone) |
| Add a device | `POST /device/authorize/start` | JSON body `{"fresh_grant": ...}` | No |
| Add a device | `POST /device/requests/{id}/pake` | JSON body `fresh_grant` | No |
| Add a device | `POST /device/requests/{id}/result` | JSON body `fresh_grant` | No |

The check is the same everywhere (`require_fresh_grant`): present,
well-formed, tagged by this server, unexpired, issued for the bearer's
account, at the account's CURRENT key generation (equal). It runs first in
each handler, before any other field is read, any limiter charged or any
row written, so a refusal costs one HMAC and one indexed read and changes
nothing.

**Reset**: semantics unchanged from 2026-10-04, the field renamed, and a
passkey-minted grant is accepted like a password-minted one. (A reset still
needs no ADR-0014 passkey action grant.)

**Delete account.** Until now an account without a passkey was deleted on
the session alone. Every account now needs the fresh-auth grant, in a header
because a `DELETE` with a body is not portable across the client HTTP
stacks; an account with a passkey needs the ADR-0014 action grant as well,
as before. The fresh-auth grant is checked first, so a request without one
does not consume the action grant.

**Add a device.** Three bearer routes let a new device in, and each is
gated:

- `POST /device/authorize/start` mints the device grant a new device redeems
  for a session (QR pairing and the confirmation flow both end here). It had
  no body; it now takes `{"fresh_grant": ...}`, and an empty or non-JSON
  body is the same `403`. Refused before `Scope::DeviceGrantMint` is
  charged.
- `POST /device/requests/{id}/pake` is the earliest step that commits the
  account's device to a camera-less exchange: it is written once and turns a
  stranger's request into one live guess at the code. Gating it means a
  stolen session or an unlocked phone cannot answer a request.
- `POST /device/requests/{id}/result` records `accepted` or `wrong_code`.
  Gating it means the same session can neither accept nor record a wrong
  code, so a refusal never counts against the account's device-link
  wrong-code budget (migration `0025`).

`GET /device/requests` (see the open request) and `DELETE
/device/requests/{id}` (decline) need the session only: neither lets a
device in. The anonymous routes of the new device are unchanged. A
fresh-auth grant is not spent by adding a device: it serves `pake`,
`result` and `authorize/start` of one exchange, and a second device, until
it expires or a key reset replaces its generation. The per-address bucket of
`/device/authorize/start` is a route layer and is still charged by a
refused call (in memory, per address, not the owner's budget).

**What the clients owe (unbuilt).** Keep the grant in memory beside the
sign-in that returned it, never in durable storage. Before any of the three
actions, if there is no grant or `fresh_grant_expires_at_ms` has passed, ask
for the password or the passkey and sign in again
(`/login/{start,finish}` or `/webauthn/auth/{start,finish}`); the same on
`403 fresh-auth-required`. A device that signs in again gets a new session
pair as well as the grant.

**Residual.** Someone holding a session AND the password or a passkey can
do all three; that is the owner as far as the server can tell. A grant
lifted from a device's memory together with the session is, for its
remaining lifetime, as good as the password for one reset or delete and for
any number of device adds within the mint limit (10 per 15 minutes by
default) and the wrong-code budget. A person whose only remaining factors
are TOTP or the recovery phrase and who has no passkey must know the
password to do any of the three (the phrase can set a new password through
`/password/reset/*`, after which a password login mints a grant).
`DELETE /discovery/keypackage/device/{id}` and the other tiers' own deletes
are not gated by this grant: it is verified by auth alone.

**Tests** (gated on `LAKKO_AUTH_TEST_DATABASE_URL`).
`services/auth/src/opaque.rs`: `deleting_an_account_needs_a_fresh_grant`
(no grant and nine unusable grants, another account's session, the grant in
a body instead of the header: `403`, the account intact; a password-minted
grant deletes; a grant from before a key reset and its sibling refused
after it) and `adding_a_device_needs_a_fresh_grant` (the same matrix on
`authorize/start`, `pake` and `result`, an empty and a non-object body,
nothing written, no wrong code logged, the two-mint budget intact; then
answer, wrong code, a second request and two mints with ONE grant; the
redeemed device's session carries no grant; after a key reset the grant is
refused and a new one works). `services/auth/src/passkey_vault.rs`:
`a_passkey_sign_in_mints_a_fresh_grant_that_adds_a_device_and_deletes_the_account`
(a real software-authenticator sign-in mints a grant that verifies; two
device grants with it; delete refused with the fresh-auth grant alone and
with the action grant alone, done with both) and the existing account-delete
case now proves a refused delete leaves the action grant unspent. The
device-link and device-authorize suites obtain a grant first. A successful
`/totp/recover` cannot be driven by the harness; that it mints no grant
rests on its response type holding `SessionPair` only.

**Added the same day: adding a passkey needs the grant, and signup mints
one.** Built in `services/auth` (DB-gated route tests; nothing walked live;
clients not wired). Where this paragraph and the ones above differ, this one
holds.

- `POST /webauthn/register/start` and `POST /webauthn/register/finish` take
  `fresh_grant` in the JSON body and run `require_fresh_grant` first, before
  the body's other fields are validated, any row is read or written, and
  with no limiter involved: `403 {"error":"fresh-auth-required"}`
  otherwise. Reason: a passkey sign-in mints a grant, so a bare session that
  could register its own passkey could then sign in with it and mint the
  proof for the reset, the delete and the device add. The finish is gated as
  well as the start so a pending registration does not outlive the grant
  that opened it (a key reset in between stops it). Not spent by a
  registration. Passkey registration is the only bearer route that creates
  a credential able to mint a grant: `/totp/enroll/*` creates a factor
  whose recovery mints none, `PUT /auth/login-handle` renames the account
  but proves nothing, `PUT /vault/passkey` stores a blob for an existing
  passkey, and the password is replaced only through the anonymous
  `/password/reset/*` (the recovery phrase) or the gated key reset.
- `POST /signup/finish` now answers `fresh_grant` +
  `fresh_grant_expires_at_ms` beside the session pair, at the genesis
  generation: the person typed the new password seconds ago, and onboarding
  offers "add a passkey" right after signup. This replaces `/signup/finish`
  in the "no other route" list above; the minting routes are three
  (`/login/finish`, `/webauthn/auth/finish`, `/signup/finish`). The grant is
  a separate flattened field of the signup answer; the shared `SessionPair`
  still cannot carry one. A guest signup (ADR-0032) is the same route on the
  server, so it gets a grant too. Consequence: for ten minutes after signup
  the new session can also reset keys, delete the account and add a device
  without typing the password again.
- Tests (`services/auth/src/opaque.rs`):
  `adding_a_passkey_needs_a_fresh_grant` (no grant, nine unusable grants and
  another account's session on the start, nothing stored; an invalid start
  without a grant is the `403`, not a `400`; the same matrix on the finish
  of a ceremony opened with a good grant, no passkey stored and the pending
  state kept; then one passkey each with a signup-minted grant, a
  password-minted grant used twice, and a passkey-minted grant from a real
  software-authenticator sign-in) and
  `only_the_password_login_and_the_signup_mint_a_grant` (renamed); every
  account the suite signs up asserts the signup grant verifies. No existing
  test drove the registration routes: the other suites insert the
  credential row directly.

**Changed the same day: an account with a passkey is deleted on the action
grant alone.** Where this paragraph and the ones above differ (the table
row, "Delete account", the delete cases of the tests paragraph), this one
holds. `DELETE /account` asks for ONE fresh proof, chosen by whether the
account has a passkey:

- **It has a passkey:** the ADR-0014 action grant bound to the
  delete-account action, and nothing else. `x-lakko-fresh-grant` is not
  read: absent, valid, expired or malformed, it neither refuses a delete the
  action grant authorises nor replaces a missing action grant (`403`
  plain-text `action-authorization-required`).
- **It has no passkey:** unchanged, the fresh-auth grant in the header, else
  `403 {"error":"fresh-auth-required"}`.

Reason: requiring both cost a passkey account two passkey gestures for one
delete (a sign-in to mint the fresh-auth grant, then the action ceremony),
and the action grant is already the stronger proof. Its only writer is
`POST /auth/action/finish`, after a verified WebAuthn assertion by one of
the account's passkeys over a challenge derived from the action hash
(`/auth/action/start` refuses an account without a passkey), so a bare
session cannot obtain one; it lives 120 seconds, is single-use and names
the account and the action. Consequence: for an account with a passkey the
password is not a way to delete it (remove the passkeys first, or use one).
Tests (`services/auth/src/passkey_vault.rs`): the vault suite's delete case
deletes with the action grant and no header;
`a_passkey_sign_in_mints_a_fresh_grant_that_adds_a_device_and_deletes_the_account`
refuses the delete without the action grant for no header, a valid
passkey-minted grant and a malformed one, and with an action grant bound to
another action, then deletes with the right action grant and a malformed
header. `deleting_an_account_needs_a_fresh_grant` (no passkey) is unchanged.

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
  conversation leaves until an admin among the person's contacts evicts
  them (phase 2, in code since 2026-10-04, not walked), and keep them in a
  group the old identity owned, which can only be recreated; owned channels
  are orphaned by a reset; a weeks-old `publish-owed` reset asks for a sign-in.
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
