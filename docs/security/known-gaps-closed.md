<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Closed gaps

Entries moved out of [`known-gaps.md`](./known-gaps.md) on 2026-10-13 because their
status names nothing left open. The text of each entry is unchanged; only the heading
was shortened and the status moved to its own line. Numbers are never reused, and the
summary table in `known-gaps.md` lists every gap with the file it is in.

## MLS state machine

<a id="g-35"></a>
### G-35: MLS group-commit publish-before-persist

**Status:** Deterministic single-device epoch-desync brick. **CLOSED (2026-07, [ADR-0040](../adr/0040-durable-pending-commit-outbox.md) durable pending-commit outbox, S2–S5 landed on web + iOS + Android)**

The G-34 T2 fix reordered the chat **send** path to
`create → persist(durable, fail-closed) → publish`, and it explicitly named the
residual it did not close (this entry above, `known-gaps.md` G-34 §T2): the four
group-membership **commit** paths — add / remove / role-change / external-join
approve — share the same publish-before-persist ordering, but with a **DISTINCT and
worse** failure mode than a same-generation nonce reuse. This entry records that
hazard and its accepted fix. Like G-33 / G-34, this is a **free-form audit record**
— it adds **no** indexed C / I / T invariant (the index stood at **8 I + 58 C + 15 T**
when this was written; the live counts live only in the gated Quick stats table in
[`invariant-index.md`](./invariant-index.md)).

**The hazard (deterministic, verified firsthand).** Each commit path runs
`lakko-mls` create-commit, which **eagerly merges** epoch N → N+1 into the in-memory
provider *inside* the create call (`crates/lakko-mls/src/group.rs`:
`merge_pending_commit` at `:280` add, `:361` remove, `:392` role/GCE;
`approve_external_join` `:498` delegates to `add_member` at `:516`), then the client
publishes the commit (+ a `WelcomeRendezvous` for add / approve) to the DS, then
persists the N+1 snapshot **best-effort** (failures swallowed — web
`contacts/client.ts` `persistBestEffort` `:443`, iOS / Android
`persistSnapshotBestEffort`). The OpenMLS provider is **in-memory**
(`identity.rs:56`, "in-memory for Phase 0"), so the snapshot is the only durable
epoch copy. A crash **or** a silent best-effort persist failure **between publish
and persist** makes the commit C durable on the DS (peers advance to N+1) while the
local snapshot rolls back to N. On restart the device does **not** self-heal: the
in-memory own-published echo-suppression set is lost (web `chat/client.ts:248`
`ownPublishedIds`, iOS `ChatClient.swift:57`, Android `ChatClient.kt:88`), so the
poll loop re-fetches C and OpenMLS **unconditionally rejects processing one's own
commit** (`own_leaf_index() == sender leaf` → `CannotDecryptOwnMessage`,
`openmls-0.8.1/src/framing/validation.rs:111-112`). The device is **permanently
stuck at epoch N** for that group — silent, no error surfaced, recoverable only by
another admin re-adding it. **Blast radius: one device (the committer).** There is
also a residual **2^-32** handshake-ratchet nonce-reuse surface if the bricked
device issues a **second** commit at N (bounded by the MLS `reuse_guard`, RFC 9420
§6.3.2) — secondary to the deterministic desync.

**Why the naive fix is worse (the load-bearing point).** The G-34 T2 shape (a bare
persist-before-publish reorder) works for **application messages** because a skipped
ratchet generation is harmless (receivers tolerate gaps). A **commit cannot be
skipped** — N → N+1 is a group-wide barrier — so a bare reorder here merely **flips**
the desync to **device-AHEAD**: the device is durably at N+1 while C never reached
the DS and C's bytes are unrecoverable from the merged snapshot, stranding the
**WHOLE group** at N with no path forward — arguably worse than the single-device
brick. The fix therefore needs **durability of the pending commit**, not an ordering
change. (Contrast: the DeviceGroup pairing path already uses persist-before-publish,
G-5 / G-7, and is safe there because the ahead-state self-heals via joiner-retry and
the single-founder case has no other member — neither holds for a chat membership
commit.)

**The accepted fix — ADR-0040 Option A: a durable pending-commit outbox
(client-only, no `lakko-mls` core / FFI change).** Keep the eager merge. Persist a
per-`(account, group)` outbox entry holding the commit wire-bytes **and** the
Welcome/rendezvous wire-bytes (the two-artifact coverage), marked un-acked,
**together with** the N+1 snapshot in one atomic write **before any publish**
(fail-closed — a failed write aborts the op with no publish, and the device rolls
back cleanly to N); publish; mark acked; and on restart **re-publish** un-acked
entries idempotently (peers at N+1 drop the duplicate as wrong-epoch; the committer
re-arms its echo filter). This makes the device never durably ahead-without-publish
nor behind-with-publish, so every crash-window outcome is either "clean N, no op" or
"N+1 with a re-publishable commit." Alternatives rejected: **Option B** (defer the
eager merge in the core until publish is confirmed — cleaner but touches the
sacrosanct MLS core + FFI, deferred as a future refinement); the **bare reorder**
(flips to the whole-group brick); and **do-nothing** (deterministic silent brick).
Strengthens **invariant #4** (no group-epoch advance published without durable local
recording of the commit); invariants #1 / #2 / #3 hold (the outbox stores opaque
commit + Welcome artefacts; no `lakko-mls` state-machine change).

**Status: CLOSED (2026-07).** ADR-0040 Option A landed end-to-end on all three
clients. **Web (S2)** — `apps/web/src/chat/commit-outbox.ts` (a durable per-`(account,
group)` store in the `lakko-mls` IndexedDB, bumped v1 → v2) + `mls-store.ts`
`saveSnapshotWithCommitEntry` writing the outbox entry + N+1 snapshot in **one atomic
`transaction([snapshots, commit-outbox])`** (Open Question 1 resolved: a single
cross-store IDB transaction, so "N+1 snapshot with no entry" is impossible and the
sweep needs no torn-write reconciliation) + the four commit-path reorders in
`contacts/client.ts` + `sweepPendingCommits` in `Chat.tsx onMount` before the first
poll. **iOS (S3)** — `apps/ios/Lakko/Chat/CommitOutbox.swift` (one JSON file per
in-flight commit under Application Support, backup-excluded) + `ChatRepository`
entry-first-then-snapshot + the `planSweep` torn-write discard + a once-per-session
sweep at the top of `pollGroup`. **Android (S4)** — `apps/android/.../chat/CommitOutbox.kt`
(file-per-entry under `noBackupFilesDir`) + `runCrashSafeCommit` + the `reconcileVerdict`
torn-write discard + `sweepPendingCommits` at group-open. Open Question 1 (step-2
atomicity) resolved per-platform: web = one atomic IDB transaction; iOS / Android =
entry-first-then-snapshot with a restart-sweep **discard** of any un-acked entry whose
durably-restored `Group.epoch()` did not reach the entry's `target_epoch` (a torn
write whose commit was never published → drop the orphan, stay at N clean). Open
Question 2 (orphan `messageId`) resolved via the **already-present** receive-loop
benign-drop of a re-fetched own commit (`CannotDecryptOwnMessage`: web
`chat/client.ts` catch-and-continue; iOS `ChatClient.swift:247` catch-all; Android
`ChatClient.kt:277` `FfiException` catch) rather than recording the `messageId` in the
durable write — the sweep additionally re-arms the echo filter for the re-published id.
The two-artifact add / approve paths carry both the commit and the Welcome (add →
mailbox) / rendezvous (approve → group journal); the sweep re-publishes each to its
destination and echo-re-arms only the group-journal artefacts. No `lakko-mls` core /
FFI change (invariant #3 untouched); no indexed invariant added. Verified firsthand:
web `tsc` clean + vitest 338 (15 new) + vite build; iOS `xcodebuild` build + test 12/12;
Android `:app:compileDebugKotlin` + `CommitOutboxTest` 10/10. Residual: a live
crash-injection / power-loss e2e on a deployed multi-device environment (a runtime
gate, like G-26) — the reorder + torn-write + sweep are proven by the per-platform
unit suites. Option B (defer the eager merge in the MLS core until publish is
confirmed) stays deferred as a future refinement.

## Multi-device

<a id="g-5"></a>
### G-5: Commit broadcast on `add_device_from_wire` to other pre-paired devices

**Status:** **CLOSED**

**Closed (2026-06-03).** All three `runScanFlow` /
`PairingOrchestrator` implementations now broadcast the
`MemberAddition.commit` to the DeviceGroup DS journal
(`DsClient.publishGroup(group_id, commit)`) right after the
Welcome PUT, mirroring the established `removeDeviceAt`
broadcast. Other already-paired devices advance to the new
epoch when their DeviceGroup poll loop runs `processIncoming`
on the row (web `DeviceGroupSyncContext`, native lifecycle
poll). MLS requires in-order epoch processing, so the
broadcast is on the critical path (not best-effort) — a
missed commit would permanently strand a third device. For
the founder + first-pair case there is no other member, so
the row is a harmless no-op. Touchpoints:
`apps/web/src/devices/orchestrator.ts`,
`apps/ios/Lakko/Devices/PairingOrchestrator.swift`,
`apps/android/.../devices/PairingOrchestrator.kt`.

<a id="g-7"></a>
### G-7: DeviceGroup snapshot persistence may briefly diverge across in-flight ratchet operations

**Status:** **CLOSED**

**Closed (2026-06-03).** The scan flow was reordered to
**persist-before-publish** on all three clients:
`add_device_from_wire` (merges the pending commit, advancing
the local view one epoch) → **persist snapshot** → publish
Welcome → broadcast commit (G-5) → best-effort history
bundle. Writing the advanced snapshot to disk before any
publish closes the window where a crash between the publishes
and the persist left the on-disk envelope an epoch behind
(which would make the next launch attempt to add the same
device twice). A crash after persist but before the publishes
leaves the on-disk state correctly advanced; the joiner
simply never receives the Welcome and retries the QR with a
fresh KeyPackage. Touchpoints: the three
`runScanFlow` / `PairingOrchestrator` files (see G-5).

## Web client

<a id="g-8"></a>
### G-8: Web message-log change listener

**Status:** **CLOSED**

The unread badge (in `Contacts.tsx`) was sourced from a
one-shot `loadMessageLog(...)` on mount, so a message
written from another tab / the Chat route did not refresh
it until the next mount. A `BroadcastChannel`-backed pub/sub
now closes the gap (`apps/web/src/chat/message-log-events.ts`):
`saveMessageLog` calls `publishMessageLogChanged(accountIdB64)`
after every write, and the badge subscribes via
`subscribeMessageLogChanged` and reloads (filtered to the
active account). `BroadcastChannel` carries the signal across
tabs; because it never echoes to the posting instance, the
publisher also notifies in-process subscribers directly, so the
same-tab / different-route case works too. Degrades to a no-op
where `BroadcastChannel` is absent (the in-process path still
fires). This brings web to parity with the iOS / Android
`@Published` / `StateFlow` in-memory stores. Verified: web
`tsc --noEmit` + `vite build` green.

<a id="g-9"></a>
### G-9: Web `payload_bytes` JSON placeholder strings are NOT byte-stable

**Status:** **CLOSED**

**Closed (2026-06-04).** `encodeMessagePlaceholder`
([`apps/web/src/devices/history-bundle.ts`](../../apps/web/src/devices/history-bundle.ts))
now serializes via a recursive sorted-key `canonicalJson`
helper instead of `JSON.stringify(m)` (whose key order is
insertion order, not byte-stable across JS engines — a
Chrome-scanner → Safari-joiner pairing on the same account is
a real case). Every `PersistedMessage` field is a string, so
recursive key sorting is the whole of RFC 8785 / JCS that
applies to this shape (no numeric canonicalization needed);
the doc-comment flags that a future numeric field would
require ECMAScript number canonicalization. Decode is
unaffected — `JSON.parse` is order-tolerant — so this is a
pure determinism improvement with no wire-compat change for
the web↔web path. `tsc --noEmit` clean. This matches the
canonical-encoding convention iOS already uses
(`JSONEncoder.outputFormatting = [.sortedKeys]`).

Note: this closes byte-stability of the **web-emitted** bytes;
it does NOT address cross-platform web↔native interop, which
is a deeper, separate problem tracked under G-6 below.

## Backup

<a id="g-10"></a>
### G-10: Backup envelope rotation folded into the recover round-trip

**Status:** **CLOSED**

After `/recover` rotates the OPAQUE envelope on the
server, the backup baseline upload re-seals the backup
envelope under the new exportKey-derived material. That
upload used to be **fire-and-forget** (web: an un-awaited
`void (async () => …)()` IIFE; iOS: a detached `Task { … }`),
so `recover()` resolved before it landed — leaving a window
in which the server still held the pre-recover envelope
("old phrase still recovers").

The baseline write is now **awaited inside the recover
round-trip** on every platform, so `recover()` does not
resolve until the re-seal has been posted (collapsing the
window to the upload latency): web `AuthContext.recover`
awaits the restore-then-bootstrap IIFE (with the bootstrap
wrapped best-effort so a backup hiccup still cannot fail the
recover); iOS `AuthRepository.recover` calls
`bootstrapBackupBestEffort` directly instead of in a detached
`Task` (it is non-throwing, so awaiting cannot fail recover).
Android already awaited it (a direct `suspend` call in the
`recover` coroutine), so it needed no change. Verified: web
`tsc --noEmit` + iOS `xcodebuild` green.

<a id="g-11"></a>
### G-11: No tamper-evident chain across backup versions

**Status:** **CLOSED IN PRACTICE (2026-07, [ADR-0037](../adr/0037-backup-head-rollback-commitment.md))**

**Closed (2026-07, [ADR-0037](../adr/0037-backup-head-rollback-commitment.md)).**
Both the returning-client and the fresh-install rollback
cases are closed in practice by one auth-signed
**backup-head** commitment minted on every upload and
verified on every restore.

**Correction to the prior claim.** This entry used to
credit the "client-with-state case" as CLOSED via the v2
chain ([`crates/lakko-backup/src/chain.rs`](../../crates/lakko-backup/src/chain.rs)).
That closure was only *crate-level*: the v2 chain is
**dormant** — it is exposed on neither FFI surface, no
client persists a `last_known_version` anchor, and no
client calls `verify_chain`, so it closed nothing on any
deployed path. ADR-0037 is what closes the case in
practice, via the head — not the chain. The v2 chain
stays **dormant / out of scope**; its distinct
sequence-audit role (walking a *served sequence* of
multiple versions, checking each `prev_hash` link) is a
separate forensics concern this gap does not need.

**How each case is closed.** A `BackupHead` is a compact,
domain-separated, Ed25519-signed blob committing
`(account_id, backup_version, envelope_hash, signed_at)`,
where `backup_version` is an **auth-assigned, per-account,
strictly-monotone** counter (never client-supplied, so a
regression is structurally impossible):

- **Returning client (ever persisted a floor) — HARD-closed.**
  The client stores `last_known_version` in
  device-key-encrypted local storage and refuses any head
  below it — a hard monotonic floor, no window. Auth
  guarantees the version never regresses, so the storage
  tier cannot serve an old head with a spuriously-high
  version to defeat the floor.
- **True zero-anchor fresh install — BOUNDED by freshness.**
  With no floor to compare against, the client falls back
  to the head's signed `signed_at` and a client-side
  staleness window (default 30 days): a rollback to a head
  older than the window is caught; a replay *within* the
  window is not.

**Residual, stated plainly.** A rollback that replays a
genuine *older* head whose `signed_at` falls **inside** the
window is **not** caught — but **only for a true
zero-anchor fresh install** (a genuine lost-everything
recovery). Any client that ever persisted a version floor
is hard-protected regardless of the window. Fully closing
the fresh-install window would require the client to read a
*trusted* current-version marker on restore (e.g. auth
serving its counter over the authenticated channel), a
second trusted round-trip ADR-0037's single-fetch design
deliberately declines. The fail-open on an **absent** head
(a legacy / pre-rollout blob) is a rollback-detection
availability choice only; the envelope stays phrase-AEAD
sealed regardless of the head, so invariant #4 (no silent
downgrade to non-E2EE) is untouched.

**Post-ADR-0037 `lakko-security` pass (APPROVE-WITH-NOTES).**
The review found no CRITICAL / HIGH and no invariant-#4 break
on the live path; the F1–F5b hardenings that landed from it
tighten the gate so the code now matches the residual wording
above rather than being looser than it. **F4** (all three
clients): the restore gate's ABSENT-head case no longer
silently fail-opens for a *returning* client. New policy —
absent head + no floor → fail-open (a true zero-anchor fresh
install, unchanged); absent head + a persisted floor +
`BLAKE3(legacy envelope) == floor.envelope_hash` → proceed
(the head was stripped benignly in transit, byte-identical to
the last known one); absent head + a floor + a **different**
envelope → user-acknowledged stale-override (a head-stripping
downgrade against head-protected history). So the earlier code,
in which the absent path bypassed the floor, is corrected — the
within-window fail-open residual is now genuinely confined to a
**true zero-anchor fresh install**, exactly as this entry's
"any client that ever persisted a floor is hard-protected"
claim already stated. **F5a** (all three clients): the floor
read distinguishes absent vs present vs **corrupt** — a
present-but-undecryptable / tampered floor routes to
stale-override, never a silent demote to the weaker
fresh-window path. **F2** (all three clients): the restore gate
is now mandatory (the ungated no-verification restore path was
removed). **F1** (all three clients): a loud warning when the
head is verified against an unpinned (JWKS-served) attest key.
**F3** (Android only): the `parseBackupBlob` catch widened to
`Throwable` — a clean hard-fail, not an uncaught escape. **F5b**
(auth): a per-account, **fail-open** rate-limit on
`POST /auth/backup/head` (`backup_head_mint_limits`, migration
`0016`), shipped **DEFAULT-OFF** (`LAKKO_BACKUP_HEAD_MINT_RATE_LIMIT`
default `0`; armed with a positive value only after live-DB
verification of the window-reset upsert).

Touchpoints: `crates/lakko-attest/src/backup_head.rs`
(`BackupHead` primitive, 129-byte layout, domain
`lakko/backup-head/v1`);
[`services/auth`](../../services/auth/src/backup_head.rs)
`POST /auth/backup/head` (stateful mint) + the
`backup_head_versions` monotone-counter table (migration
`0015`); `crates/lakko-backup/src/head_frame.rs` (the
`0xB1` container-tag framing embedding the head in the one
opaque storage blob — `services/storage` untouched); the
three-client restore gate (web
[`head-gate.ts`](../../apps/web/src/backup/head-gate.ts),
iOS `BackupHeadGate.swift`, Android `HeadGate.kt`) applying
the absent → fail-open / invalid → hard-fail / stale →
user-acknowledged-override policy. Fuzz:
`lakko-attest/backup_head_open` +
`lakko-backup/backup_blob_parse`. See C-58 in the
invariant index. Residual for a full close: the deferred
auth-served current-version marker (above) + a live
recovery e2e (mint → framed PUT → restore gate) on a
deployed environment.

## Auth

<a id="g-12"></a>
### G-12: Session token validation is single-process in-memory

**Status:** **CLOSED (was already addressed; text was stale)**

**Closed (2026-06-03) — verified already implemented.** The
described per-process in-memory bearer-token map no longer
exists (and a code audit confirms there is none anywhere in
`services/auth` / `services/discovery` — the only `HashMap`
is discovery's IP rate-limit buckets). Session tokens are the
**stateless** HMAC-SHA256 shape in
[`crates/lakko-auth-token`](../../crates/lakko-auth-token/src/lib.rs):
`v1.<account_id_b64>.<expires_at_ms>.<tag>`, where the tag is
HMAC-SHA256 over the prefix under a 32+-byte secret loaded
from the shared `LAKKO_SESSION_TOKEN_SECRET` env var. Both
`services/auth` (issue + `webauthn.rs` / `opaque.rs` verify)
and `services/discovery` (`auth.rs` verify) load the SAME
secret, so:

- **Any instance validates without coordination** — exactly
  G-12's ask. No DB row, no shared cache, no sticky sessions.
- **Restarts don't invalidate tokens** — the secret is
  config, not process state.

Verification is constant-time (`subtle::ConstantTimeEq`),
version-pinned (`v1`), and expiry-checked; 11 unit tests
cover round-trip, wrong-secret, tamper, expiry, malformed
shapes. The one explicit non-goal documented in the crate is
a **revocation list** (a leaked token is valid until its TTL
expires) — that lands "when the threat model expects targeted
token-theft (Phase 2+)" and is a distinct concern from the
"single-process" claim this gap raised.

<a id="g-13"></a>
### G-13: WebAuthn passkey per-credential authentic-action attestation

**Status:** **CLOSED**

**Closed.** WebAuthn action-binding landed via
[ADR-0014](../adr/0014-webauthn-action-binding.md) (challenge =
`BLAKE3_keyed(action_hash ‖ nonce)`, `POST /auth/action/{start,finish}`,
single-use server-side grants gating delete-passkey, client `assertAction`
on web + iOS + Android) and the device-removal half via
[ADR-0015](../adr/0015-device-removal-authorization.md) (peer-verified
`lakko-attest` Ed25519 attestation carried alongside the DeviceGroup Remove
commit + verified in the `lakko-mls` apply path, auto-enabled on first
passkey). The original framing below predates that work.

Historical context: the passkey assertion validated the user has the
device; it didn't bind to the specific OPAQUE message the request carries. A
reorder attack inside the same TLS session is implausible but not impossible
— Phase 2c can mix the OPAQUE message hash into the WebAuthn assertion
challenge.

**Server-side implementation landed (2026-06-04) —
[ADR-0014](../adr/0014-webauthn-action-binding.md).** A spike
confirmed the cryptographic binding is not a small change:
`webauthn-rs 0.5.5` (and the low-level `webauthn-rs-core`)
randomise the challenge internally with no injection hook, and
`AuthenticationState.challenge` is `pub(crate)` — so
RP-controlled `challenge = H(action_hash ‖ nonce)` requires
either owning verification or a fork. ADR-0014 chose a
**minimal vendored fork** of `webauthn-rs-core`
(`crates/vendor/webauthn-rs-core`, MPL-2.0, consumed via
`[patch.crates-io]`) that adds one challenge-injection method
(`generate_challenge_authenticate_with_challenge`, takes the
challenge as raw `Vec<u8>`) while reusing upstream's
`authenticate_credential` verification **verbatim**. Shipped in
`services/auth`:

- the action-challenge derivation primitive
  `BLAKE3_keyed(derive_key("lakko/webauthn-action/v1"),
  action_hash ‖ server_nonce_32)` (`webauthn_action.rs`);
- `start_action_auth` / `finish_action_auth` over the fork;
- the bearer-authenticated `POST /auth/action/{start,finish}`
  endpoint pair, backed by migration `0008_webauthn_action.sql`
  (`webauthn_states.action_hash`, kind = 3) with single-use
  state deletion, account binding, and a defence-in-depth
  `action_hash` re-check on `/finish`;
- property tests driving a real software authenticator
  (`webauthn-authenticator-rs` `SoftPasskey`, dev-only) that
  prove a correctly-bound assertion verifies and an assertion
  produced for action A is **rejected** on action B's session.

The existing username/discoverable login flows stay on the
safe high-level `Webauthn`. The fork-delta guard (ADR
test-plan item 5) landed as
`services/auth/tests/fork_delta_guard.rs`: it pins the SHA-256
of the vendored `core.rs` plus structural assertions, so any
edit to the verification path fails CI.

**Client-side wiring landed** (web / iOS / Android) for two
sensitive actions, each gated on a fresh passkey gesture
cryptographically bound to the action via
`action_hash = SHA-256("lakko/action/<op>/v1" ‖ identifier)`:
*delete-passkey* (identifier = credential id) is a strict gate;
*remove-device* (identifier = the removed DeviceGroup leaf's
MLS signature public key) is conditional — required only when
the account has a passkey (a 404 / `NoCredentials` from the
ceremony start means OPAQUE-only → proceed), and fail-closed on
a user-cancelled gesture. `assertAction` reuses each platform's
existing passkey-assertion machinery (web `navigator.
credentials.get`, iOS `ASAuthorizationController`, Android
`CredentialManager`).

**Server-side enforcement landed for delete-passkey.** The gate
is no longer client-side-only there: `/auth/action/finish` writes
a short-lived single-use grant `(account_id, action_hash)`
(table `webauthn_action_grants`, migration 0009, TTL 120 s,
pruned on the existing timer), and `DELETE
/webauthn/credentials/:id` recomputes `action_hash =
SHA-256("lakko/action/delete-credential/v1" ‖ credential_id)`,
atomically consumes a matching grant (`DELETE … rows_affected ==
1`, so it is race-free + strictly single-use), and refuses with
403 `action-authorization-required` if none is present. A stolen
session token alone cannot mint a grant (that needs a passkey
gesture at `/finish`), so the gate now resists token replay. The
no-op case (deleting a credential that does not exist) stays an
idempotent 204 with no grant required. A unit test pins the
server's `delete_credential_action_hash` to the clients'
convention so the two cannot drift, and a DB-gated HTTP
integration test (`action_grant_e2e_tests`, gated on
`LAKKO_AUTH_TEST_DATABASE_URL`, skips when no DB is provided)
drives a real `SoftPasskey` authenticator through the production
router end-to-end: it proves a ceremony writes a grant, the
delete consumes it (204), a delete without a grant is refused
(403), and a non-existent credential is an idempotent 204.

**Remove-device — analysed in [ADR-0015](../adr/0015-device-removal-authorization.md);
peer-verified attestation in progress.** A device removal is an
MLS `Remove` commit signed by the removing device's per-device
MLS signature key, whose seed is stored encrypted under a
non-extractable device key (invariant #7). So a remote attacker
holding only a stolen session token **cannot** forge a removal —
peers reject an invalidly-signed commit — which already closes the
token-replay threat that motivated the delete-passkey grant. The
DS is zero-trust opaque and cannot interpret an `action_hash`, and
the auth service is never in the removal path, so there is no
server request to gate. The **residual** the current client-side
gate does not cover is **on-device malware that bypasses the app
UI and drives the MLS FFI directly**.

ADR-0015 was accepted to close that residual via **peer-verified
attestation**: `services/auth` signs a compact Ed25519 attestation
over `(account_id, action_hash, expiry)` on `/auth/action/finish`,
the removing device carries it alongside its `Remove` commit, and
the receiving devices verify it in the `lakko-mls` apply path
before applying. **Sprints 1–2 landed.** Sprint 1: the
`lakko-attest` crate (`ActionAttestation` format + `sign` +
strict `verify_bound`, 7 unit/property tests). Sprint 2: a
dedicated Ed25519 attestation key in `services/auth` (migration
0010 `action_attest_signing_key`, generated on first boot,
separate from the call-JWT key), `/auth/action/finish` now signs
+ returns the attestation (`attestation_b64`, 1 h TTL), and the
verifying key is published at `GET
/.well-known/device-attest-jwks.json` — the DB-gated HTTP e2e test
verifies the issued attestation under the published key + its
(account, action) binding. The **sprint-4 OpenMLS feasibility
spike** also ran and **confirmed** the OPAQUE-only policy flag is
viable: `device_group.rs::spike_custom_group_context_extension_propagates_to_joiners`
proves OpenMLS 0.8.1 carries a custom `Extension::Unknown` in the
GroupContext, reads it back, and propagates it to a joining
device — so the application-level fallback is unneeded (the one
requirement: every device's leaf capabilities must advertise the
extension type). **Sprint 4 landed** — the policy flag is wired
into `lakko-mls`: every device `KeyPackage` + `DeviceGroup`
founder now advertises the removal-policy extension capability,
DeviceGroups carry `required_capabilities` for it from creation
(so it can be added later), `DeviceGroup::set_require_attested_removals`
flips it on via a `GroupContextExtensions` commit, and
`require_attested_removals` reads it from authenticated group
state; a property test proves the flip propagates to a joined
device through `process_incoming`. (Format note: this changes
DeviceGroup creation + KeyPackages — pre-existing groups/KPs
would need re-keying to gain the capability; immaterial pre-launch.)
**Sprint 3 landed** — the apply-path verification:
`Group::process_incoming_gated` exposes a Remove commit's removed
`(leaf, signature pubkey)` pairs to a gate *before* merging, and
`DeviceGroup::process_incoming_verifying_removals` uses it to
**refuse** (not merge) a removal when `require_attested_removals`
is on and a removed leaf lacks a valid `lakko-attest` attestation
bound to this account + `SHA-256("lakko/action/remove-device/v1"
‖ removed_leaf_pubkey)` + fresh. Six property tests cover valid /
missing / wrong-leaf / wrong-key attestations, policy-off
pass-through, and non-remove-commit pass-through; a refused
removal leaves the device present (commit not merged). So
on-device malware that evicts a device without a real passkey
gesture can no longer make honest peers drop it. **Sprint 5
(FFI surface) landed** — `set_require_attested_removals`,
`require_attested_removals`, `process_incoming_verifying_removals`,
and the free `remove_device_action_hash` are exposed on both the
UniFFI (Swift / Kotlin) and wasm-bindgen (web) DeviceGroup
surfaces (regenerated bindings carry all four; FFI-crate tests
green). The **web transport primitives** are in place — a
DeviceGroup journal envelope codec (`apps/web/src/devices/journal-envelope.ts`,
`version ‖ attestation_len ‖ attestation ‖ mls_message`) that lets a
Remove commit carry its attestation through the opaque journal, a
device-attest JWKS client (`apps/web/src/auth/device-attest-key.ts`)
that fetches the 32-byte verifying key, and the `attestation_b64`
field surfaced on the `ActionFinishResponse` type. The **web
client wiring landed** — `Devices.tsx` captures the
`attestation_b64` from the remove-device ceremony, `remove.ts`
wraps it with the Remove commit, every DeviceGroup `publishGroup`
site (add / remove / application message) emits the envelope, and
the `DeviceGroupSyncContext` receive loop unwraps + applies via
`processIncomingVerifyingRemovals` against the fetched device-attest
key (a zero-key fail-closed fallback when the JWKS endpoint is
unreachable: harmless while the policy is off, refuses removals
once it is on). `tsc --noEmit` + `vite build` clean; the wasm
bindings were rebuilt to expose the methods. The **iOS + Android
client wiring also landed** — the same shape over the UniFFI
surface: a Swift / Kotlin `DeviceGroupJournalEnvelope` codec + a
`DeviceAttestKey` JWKS fetch, `DevicesScreen` captures the
attestation, `DeviceRemoval.removeDeviceAt` wraps it, every
DeviceGroup `publishGroup` site (pairing add / remove / sync app
message) emits the envelope, and the `DeviceGroupSyncRepository`
receive loop unwraps + applies via `processIncomingVerifyingRemovals`
against the fetched device-attest key (same zero-key fail-closed
fallback). `xcodebuild` (iOS, after rebuilding the
`LakkoCore.xcframework`) and `:app:compileDebugKotlin` (Android)
both green. The **policy is now turned ON automatically** — all
three clients call `setRequireAttestedRemovals` (a
GroupContextExtensions commit, broadcast over the journal,
idempotent / no-op when already on) right after a passkey is first
registered (`PasskeysSection` on web, `PasskeysViewModel` on iOS +
Android; the shared `enableAttestedRemovalsIfNeeded` helper).
G-13 is thus **implemented end-to-end** — server attestation +
peer-verified apply path (6 `lakko-mls` property tests) + the
3-platform transport + auto-enable on passkey-enable. **Key
pinning landed** — all three `DeviceAttestKey` clients accept a
build-time pinned verifying key (`VITE_LAKKO_DEVICE_ATTEST_KEY` on
web, `Info.plist` `LakkoDeviceAttestKey` on iOS,
`BuildConfig.LAKKO_DEVICE_ATTEST_KEY` on Android); when set they
**never consult the JWKS endpoint**, so a compromised
JWKS-serving tier cannot substitute a key it controls (the
key-isolation deployment posture). Unset → JWKS fetch, unchanged.
The threat boundary: this defends a compromised serving tier, not
a fully-compromised auth service that also holds the signing key
(which could mint valid attestations regardless) — the latter is
the inherent trust G-13 places in the auth service's
passkey-ceremony verification. **Residual (optional, not
blocking):** distributing the pinned key via the *signed*
bootstrap config (vouched by an independent trust anchor) instead
of a build constant — needs the client-side bootstrap-config
consumption (a Phase-5 transport concern) that does not exist
yet, so it is out of G-13's scope.

## Calls

<a id="g-17"></a>
### G-17: Frame-counter exhaustion path is test-exercised

**Status:** **CLOSED**

`ParticipantKey::seal` honours the RFC 9605 §4.4 32-bit
counter bound and surfaces `Error::CounterExhausted` when
exhausted; the caller is expected to trigger a fresh MLS
commit to re-key. A dedicated test that grinds 4 billion
seal ops is infeasible in CI; instead the test trio in
`crates/lakko-calls/src/sframe.rs` uses a `#[cfg(test)]`-
only `set_counter_for_test` accessor to land on the edge
in O(1):

- `counter_exhaustion_returns_typed_error` — counter at
  `u32::MAX - 2`: two seals succeed landing on
  `frame_counter = u32::MAX - 1` then `u32::MAX`; the
  third refuses with `Error::CounterExhausted` (no panic,
  no silent 33-bit wrap, no nonce re-use).
- `counter_max_frame_still_opens_on_receiver` — the last
  legal `frame_counter = u32::MAX` authenticates correctly
  on a receiver re-deriving the same `ParticipantKey`
  (no off-by-one in nonce composition at the boundary).
- `counter_exhaustion_is_idempotent` — 16 successive seal
  attempts past exhaustion all return the typed error and
  the counter stays at `u32::MAX` (never wraps to 0,
  which would re-use the very first nonce in this key's
  lifetime).

The test-only accessor is compiled out of the release
crate via `cfg(test)` so no production caller can reset
the counter and re-use an AEAD nonce.

<a id="g-19"></a>
### G-19: SFU-side call-admission JWT validation

**Status:** **CLOSED** (Rust SFU; LiveKit-fork plan superseded)

**Closed.** The validation side landed in the **Rust SFU** (ADR-0011, which
retired the LiveKit fork referenced below). `lakko-svc-sfu` verifies the
Ed25519 call-admission JWT against the auth service's JWKS on the
`/v1/connect` WebSocket admission path before a peer joins a call; the
dockerised `ci/sfu-e2e` gate exercises exactly this (a fixture JWT →
`Welcome` frame). The "pending 3b1c / Go-side LiveKit fork" framing below is
superseded — there is no Go verifier; the load-bearing path is the Rust
SFU's own `lakko_call_jwt::verify_with_jwks`.

Historical context (pre-ADR-0011):

**Issuance side: shipped in 3b4** — crypto in
[`crates/lakko-call-jwt/`](../../crates/lakko-call-jwt/src/lib.rs),
handlers + key persistence in
[`services/auth/src/call_jwt.rs`](../../services/auth/src/call_jwt.rs).
Mints Ed25519-signed JWTs with the C-15..C-23 invariants
in [`calls.md`](./calls.md). Public key is published at
`GET /.well-known/jwks.json`. Client-side parse + verify
is exposed through the FFI crates so a Lakko client can
self-verify a JWT before presenting it.

**Validation side: still pending 3b1c (Go-side, in the
LiveKit fork)**. The Rust-side `lakko_call_jwt::verify_with_jwks`
is functional and tested + serves as the reference
implementation the Go verifier interops against, but
isn't on the load-bearing SFU path until the LiveKit fork
consumes the JWKS doc.

Operational consequence: today's `POST /call/jwt`
endpoint issues admittedly-bearer tokens but there's no
SFU host to present them to. Clients can already cache
the JWT — the call won't actually open until the SFU is
deployed in 3b2.

<a id="g-22"></a>
### G-22: Per-joiner dedup for call invites

**Status:** **fully CLOSED ([ADR-0032](../adr/0032-multi-joiner-call-links.md)): committer-side per-signature-key admission dedup on all 3 clients via the `keyPackageSignatureKey` FFI helper**

**Landed with the ADR-0032 multi-joiner flow (2026-07)** — the acting committer
dedups admissions on `(relay from-peer-id)` + exact-KeyPackage-bytes, so one
SFU-admitted peer is added at most once even if it re-mints fresh KeyPackages
(a retry re-serves the stored Welcome); the server `max_uses` on the
`/c/<blob_id>` blob is the orthogonal ceiling. **Per-signature-key dedup landed
(2026-07):** `lakko_mls::key_package::signature_key` extracts the stable leaf
(Ed25519) key from a wire KeyPackage (via `KeyPackageIn::unverified_credential`)
+ FFI `keyPackageSignatureKey` (uniffi + wasm); all three clients now dedup
**primarily** on that stable identity key — so a peer re-minting FRESH
KeyPackages under one identity is added at most once (the two are distinct KP
bytes but share the leaf key) — with the per-`from`-peer + exact-KP-bytes guards
kept as fallbacks. The historical note below is the pre-ADR-0032 baseline.



**Atomic `max_uses` enforcement shipped in 3c3-server**
via `services/discovery/src/call_invite.rs::consume` —
the handler bumps `used_count` only when the row has
remaining budget (same CASE-expression sentinel mapping
the contact-invite consume handler uses: `0 → 1`,
`u32::MAX → unlimited`, otherwise N). Exhausted budgets
surface as `410 Gone`; expired rows fold into `404 Not
Found` to deny status-code probing.

**Per-joiner dedup shipped in 3c3-multi-use** —
[`0008_call_invite_consumers.sql`](../../services/discovery/migrations/0008_call_invite_consumers.sql)
adds the `call_invite_consumers` side table mirroring
`invite_consumers` (PK on `(blob_id, joiner_tag)`,
`FOREIGN KEY ... CASCADE` on `call_invites.blob_id`).
The consume handler runs `INSERT IGNORE` into the table
inside the same transaction as the counter bump; a
repeat from the same joiner conflicts on the PK,
surfaces as `already_consumed = true` in the response,
and does NOT bump the counter.

Joiner-tag computation reuses the existing
[`lakko_contacts::invite::compute_joiner_tag`](../../crates/lakko-contacts/src/invite.rs)
function (HMAC-SHA-256 over a per-blob salt). The salt
is `SHA-256(JOINER_TAG_SALT_DOMAIN_V1 || capability_bytes)`,
so contact-invite and call-invite tags for the same
joiner don't collide even though the helper is shared —
[`lakko_contacts::call_invite::tests::joiner_tag_works_on_call_invite_bytes`](../../crates/lakko-contacts/src/call_invite.rs)
asserts this cross-kind salt isolation.

**KP-republish orchestration for multi-joiner calls
shipped in 3c3-kp-forward + 3c3-incoming-dispatcher** —
the second-and-subsequent-joiner path now has a fully
specified wire chain: joiner wraps a fresh KeyPackage
via `wrap_call_kp_forward`, ferries it through the
shared ContactRequest envelope transport to the current
committer; the committer's polling loop runs
`dispatch_incoming_envelope` which discriminates among
welcome / KP-forward / intro-message kinds; on a KP-
forward hit the committer runs
`Group::add_member_from_wire(kp)` + commits + wraps the
resulting Welcome via `wrap_call_welcome` and ferries
it back to the new joiner. The owner-handover sprint
(C-36) extended this with the policy for "what happens
when the original committer leaves" — the owner
designated in the capability becomes the new
committer-of-record. The full multi-party + handover
flow is backed by two named integration tests:
`link_call_multi_party_e2e_via_kp_forward_envelope`
(3-party rendezvous) and
`link_call_owner_handover_after_committer_leaves`
(committer-leaves + owner takeover). The dedup table
(this G-22's primary scope) composes with the KP-
forward chain cleanly: an already-consumed
`(blob_id, joiner_tag)` short-circuits to the
"already joined" UI branch without re-bootstrapping
the MLS group.

<a id="g-31"></a>
### G-31: Cross-stream SFrame AEAD nonce reuse (multiple streams under one leaf)

**Status:** **CLOSED (2026-07, [ADR-0035](../adr/0035-sframe-per-stream-kid-key.md); [ADR-0034](../adr/0034-sframe-per-media-stream-key.md) was an incomplete first attempt, superseded)**

A `lakko-security` review of the mid-call camera-on arc
([ADR-0032](../adr/0032-multi-joiner-call-links.md) piece B)
surfaced a pre-existing, system-wide **HIGH**. The SFrame
header KID is `= sender_leaf`, and a participant sends both
audio and video under that one leaf. `CallKey::participant_key`
derived a single `(key, nonce_salt)` from `(seed, sender_leaf)`
alone, so the audio and video `ParticipantKey` instances — each
running an **independent** counter from 0 — sealed audio frame
`N` and video frame `N` under an **identical** `(key, nonce)`
pair on different plaintexts:

- ChaCha20 keystream reuse: `C_audio(n) XOR C_video(n) =
  P_audio(n) XOR P_video(n)` (leaks the XOR of the two streams).
- Poly1305 one-time-key reuse across two messages (tag forgery).

For an **SFU group video call** the zero-trust SFU relays and
holds both ciphertext streams — exactly the adversary invariant
#2 targets — so it is a real confidentiality + integrity break
(**HIGH**). A **1×1 native** call sits behind P2P DTLS-SRTP (no
server in the media path), so the reuse is defence-in-depth only
(low); **web 1×1** has no SFrame and is unaffected. Violates RFC
9605 §9 `(key, CTR)` uniqueness.

**First attempt (ADR-0034, incomplete — superseded).** ADR-0034
folded a 1-byte Audio/Video `MediaType` tag into `participant_key`.
That closed the audio-vs-video pair but **re-collided two video
streams** from one leaf: a participant publishing camera + screen-share
sends **two** `Video` streams, both mapping to the tag `0x02`, so they
re-derived the identical `(key, nonce_salt)` and their independent
counters reopened the exact HIGH reuse. A local media-type/track-kind
cannot distinguish two same-kind streams, and a transceiver MID differs
across the SFU — neither is a discriminator both endpoints share.

**Closure (ADR-0035).** The SFrame header KID becomes a
per-`(sender_leaf, stream)` identifier `KID = (sender_leaf << 8) |
stream_index` (`stream_index` a per-sender `u8` ordinal — audio 0,
camera 1, screen-share 2, …), and `CallKey::participant_key(kid)` folds
the **FULL KID** into **both** HKDF `info` strings. Distinct KID ⇒
disjoint `(key, nonce_salt)` space, so **every** concurrent stream of a
leaf keys into its own space — the reuse is closed for **any** number
of streams per leaf, fully covering the multi-video (camera +
screen-share) case ADR-0034's per-media-type fix did **not**. The KID
is the only per-stream signal both endpoints share through the SFU,
which forwards the RFC 9605 header verbatim (invariant #2); the receiver
reads the KID off each frame's header and derives `participant_key(KID)`
with **no** local media context. The `SframeHeader` codec, the
`header_decode` fuzz target, and the ADR-0013 AAD are unchanged (RFC
9605 §4.3 already encodes an arbitrary `u64` KID in an extended form).
The fix landed and is **cross-stack-verified** on all six SFrame stacks
(core, uniffi FFI, wasm FFI, native C-ABI, C++ bridge, web worker — plus
the iOS Obj-C++ shim and the Android JNI module, whose `SFrameMediaType`
mapping was deleted), each producing the same canonical golden wire
vector
`910300fe2c2af65799594a4c693ff4c9e0d49b0565756ddd7299b0945fc8adb1c9c12fced57c6937f372e6b94bdae40a`
(seed `0x42`×32, leaf 3, `stream_index 0` ⇒ KID `0x0300`). The
`MediaType` discriminator + the native `BadMediaType` status are
removed; the core fails **closed** on `stream_index > 255`
(`StreamIndexOverflow`). Property test:
`sframe::tests::three_streams_of_one_leaf_get_distinct_keys_and_nonces`
(audio + camera + screen-share). This re-words **C-5**
(`docs/security/calls.md`) to per-`(leaf, stream)`. The only remaining
care item is the sender's stable-distinct-`stream_index`-per-stream
discipline (an engineering invariant, not a crypto gap). Residual: the
same G-26 live deployed-SFU RTP interop proof that governs every SFrame
stack.

<a id="g-27"></a>
### G-27: Channel PublisherGroup

**Status:** **CLOSED** — all three platforms wired (G4-2-mls-publisher + G-27 iOS / Android slice) + persistence + rotation landed (G4-2-mls-persistence)

[`apps/web/src/channels/composer.ts::freshPublisherKeys`](../../apps/web/src/channels/composer.ts)
now derives the channel content-key seed from a real
MLS PublisherGroup exporter: `Group.create(identity)`
→ `exportSecret(identity, channelExporterLabel(),
channelIdBytes, 32)` →
`channelSeedFromMlsExporterOutput`. The identity is
rebuilt from the OPAQUE export key
(`wasm.Identity.fromSeed(secrets.exportKey)`) inside
the `NewChannel` route handler in
[`apps/web/src/routes/Channels.tsx`](../../apps/web/src/routes/Channels.tsx).
The wasm-pack bindings have been regenerated so
`channelExporterLabel` + `channelSeedFromMlsExporterOutput`
are exposed in the TypeScript `.d.ts`. TS typecheck +
`npm run build` are green.

**Closed — iOS + Android wired.** Both per-platform
composers now drive the same `Group::create` +
`Group::export_secret` +
`channelSeedFromMlsExporterOutput` triplet the web
composer uses:

- [`apps/ios/Lakko/Channels/ChannelsComposer.swift::freshPublisherKeys`](../../apps/ios/Lakko/Channels/ChannelsComposer.swift)
  now takes `(identity: Identity, channelIdHex:
  String)`, builds a transient `Group` via
  `Group.create(identity:)`, calls
  `group.exportSecret(identity:, label:
  channelExporterLabel(), context: channelId,
  length: 32)`, and crosses the FFI boundary via
  `LakkoCore.channelSeedFromMlsExporterOutput`. The
  caller plumbs `secrets.exportKey` from the
  `AuthRepository` session through
  `ChannelsRootScreen(exportKey:)` →
  `ChannelsListScreen` → `NewChannelScreen` → its
  `create()` action which constructs the throwaway
  `Identity.fromSeed(seed: exportKey)` for the
  duration of the call.
- [`apps/android/app/src/main/kotlin/me/lakko/app/channels/ChannelsComposer.kt::freshPublisherKeys`](../../apps/android/app/src/main/kotlin/me/lakko/app/channels/ChannelsComposer.kt)
  ships the same shape with the same lifecycle
  semantics — `Group` and `Identity` are both
  `AutoCloseable` via UniFFI's Disposable
  conformance, so the call sites use `try { ...
  } finally { group.close(); identity.close() }`
  to drop the Rust handles eagerly rather than
  waiting on the JVM GC. The Compose call site at
  `apps/android/.../ui/channels/ChannelsScreen.kt::ChannelsRootScreen(exportKey: ByteArray, ...)`
  threads the export key through the same shape
  as iOS.

`xcodebuild -sdk iphonesimulator -configuration Debug build`
and `./gradlew :app:assembleDebug` both green after
the wire-up. The audit-pack invariant C-44 ("the
channel seed crosses the FFI boundary via
`channelSeedFromMlsExporterOutput`, never via a raw
`Data` cast") now holds on every client platform
that ships a publisher composer.

**G4-2-mls-persistence — CLOSED (May 2026).**

The persistence + rotation gap closed across six
sub-sprints:

- **Step 1 (Rust core)** —
  `crates/lakko-mls/src/publisher_group.rs` ships
  `PublisherGroup` keyed by `channel_id`, with
  `load` / `create` / `open_or_create` routed
  through OpenMLS's `MlsGroup::new_with_group_id`
  and `MlsGroup::load`. Deterministic group id
  derivation: `prefix("lakko/publisher-group/v1/") ||
  channel_id`. 8 unit tests pin the load-or-create
  idempotence invariant. **Superseded (Aug 2026):** that
  41-byte shape could never be published — a
  PublisherGroup id is a DS journal routing key on the
  ADR-0028 OQ3 co-publisher path and `services/ds`
  accepts exactly 32 bytes. The prefix is now the 16-byte
  `b"lakko/pubgrp/v2/"`, keeping the derivation
  deterministic and invertible at the DS width; v1 state
  is detected and refused rather than silently re-minted.
  See C-52 in `calls.md`.
- **Step 2 (FFI)** — UniFFI + wasm-bindgen
  `PublisherGroup` wrappers in `lakko-ffi-uniffi` +
  `lakko-ffi-wasm`. `publisher_group_load` /
  `publisher_group_open_or_create` free functions
  (Option<Self> work-around for UniFFI 0.28). 7
  UniFFI + 4 wasm host-side tests.
- **Step 3 (composer refactor)** — iOS + Android +
  web composers all swapped throwaway `Group.create`
  for `PublisherGroup.openOrCreate`. xcodebuild +
  `:app:assembleDebug` + `npm run build` all green.
- **Step 4 (rotation)** —
  `PublisherGroup.rotate(identity)` advances the
  MLS group epoch via `MlsGroup::self_update` +
  `merge_pending_commit`. Returns the wire-encoded
  commit for fanout to co-publishers. Both the
  Rust-core and the FFI surfaces are covered by
  `rotate_advances_epoch_and_changes_seed` /
  `rotated_seed_persists_across_reload` /
  `publisher_group_rotate_changes_seed`.
- **Step 5 (audit pack)** — see C-52 in
  `docs/security/calls.md` for the persistence +
  rotation invariants.
- **Step 6 (cross-publisher membership)** —
  `rotate_emits_commit_bytes_processable_by_co_publisher`
  proves: Alice mints PublisherGroup, adds Bob via
  Add proposal, Bob joins from welcome, Alice
  rotates, Bob processes the commit, both ends at
  the same epoch with the same exporter output.
  ADR-0010 §"PublisherGroup" cross-publisher
  seed-sync invariant satisfied.

Remaining deferred follow-ups (out of G4-2 scope):

- **G4-2-rotation-timer wave 1 DONE** (May 2026) —
  scheduled rotation policy + web orchestration.
  `crates/lakko-channels/src/rotation_policy.rs` ships
  `RotationPolicy` (24 h or 1 000 posts, both
  thresholds independently disable-able) + `RotationState`
  + pure `should_rotate(now_ms, state)` decision.
  Saturating arithmetic on the time delta tolerates
  host-clock jumps backward. 13 unit tests + UniFFI +
  wasm-bindgen FFI surface + audit-pack invariant
  **C-54** in [`calls.md`](./calls.md).
  `apps/web/src/channels/rotation-cron.ts` ships
  `runRotationOnce` (single-pass: policy check → MLS
  `PublisherGroup.rotate` → fetch subscribers → seal new
  envelopes per subscriber → atomic batch `POST
  /v1/channels/:id/rotate`) + `startRotationCron`
  (periodic poller, default 5-minute heartbeat). Empty
  batches are allowed (still bump the epoch — covers
  the "no subscribers yet" case). Server-side `POST
  /v1/channels/:id/rotate` is C-42 (atomic
  rotation); client now drives it via the new
  `rotateChannel` helper at
  `apps/web/src/channels/api.ts`.
- **G4-2-rotation-timer-ios DONE** (May 2026) —
  `apps/ios/Lakko/Channels/RotationCron.swift` ships
  the Swift orchestrator as an `actor` with `runOnce(policy,
  state, nowMs?)` + `start(policy, pollInterval,
  loadState, onError)` returning a `Task` whose
  `cancel()` stops the loop. Same shape as the web side:
  policy check via UniFFI `channelShouldRotate` →
  `PublisherGroup.rotate` → fetch subscribers → seal new
  envelopes per subscriber → atomic batch via
  `ChannelsClient.rotateChannel(...)`. New
  `RotateBatchEntry` wire type lives in
  `ChannelsClient.swift` next to the existing batch
  shapes. `xcodebuild -scheme Lakko -destination
  'platform=iOS Simulator,name=iPhone 17'` →
  BUILD SUCCEEDED.
- **G4-2-rotation-timer-android DONE** (May 2026) —
  `apps/android/app/src/main/kotlin/me/lakko/app/channels/RotationCron.kt`
  ships the Kotlin orchestrator with the same shape
  (`runOnce` suspend fun + `start(scope, policy,
  loadState, pollInterval, onError)` returning a `Job`).
  `RotateBatchEntry` @Serializable wire type +
  `ChannelsClient.rotateChannel` Ktor POST. Uses the
  UniFFI bindings (`channelShouldRotate`,
  `channelRotationStateAfterRotation`,
  `sealSubscriberSeedEnvelope`) verbatim — no Kotlin-side
  re-implementation of the policy. `./gradlew
  :app:assembleDebug` → BUILD SUCCESSFUL.

The G4-2-rotation-timer arc is now complete across all
three platforms (web + iOS + Android). All decisions
about _when_ to rotate flow through one Rust file
(`crates/lakko-channels/src/rotation_policy.rs`); each
platform's orchestrator is ~200 LOC of pure plumbing
above the same FFI surface.

The cryptographic property is preserved: the seed is
genuinely derived from the MLS exporter under the
publisher's identity, not from `OsRng`. What is
missing is the multi-device byte-identicality
property (per ADR-0010 Tier 1, two devices of the
same publisher on the same channel SHOULD produce
the byte-identical seed via shared identity +
identical `Group::create`/exporter inputs;
`Group::create` uses the identity's MLS provider
which currently treats `group_id` as random, so
that property only holds after group-id persistence

<a id="g-28"></a>
### G-28: SFrame on-device wiring

**Status:** CLOSED (May 2026) via the G3-sframe-native arc (N-1..N-6)

**Status (May 2026)**: **CLOSED.** Six sub-sprints (N-1
through N-6) shipped the full native-shim stack on both
mobile platforms. The Rust SFrame algorithm is now
plumbed end-to-end through libwebrtc's
`FrameEncryptorInterface` / `FrameDecryptorInterface`
on iOS and Android, with security invariant #4 ("no
fallback to non-E2EE") enforced at five layers
(documented under C-45..C-50 in
[`docs/security/calls.md`](./calls.md)).

What landed:

- **N-1** `crates/lakko-sframe-native/` — flat C ABI
  around `lakko_calls::sframe` (11 `extern "C"`
  functions + 7 unit tests + cbindgen header emitted
  to a stable mirror at `target/lakko-include/`).
- **N-2** `apps/shared/webrtc-cpp-headers/M125/` — 13
  sha256-pinned libwebrtc M125 (branch-heads/6422)
  C++ headers + 2 Lakko-authored abseil shims +
  `fetch.sh` verify-mode + `probe.cc` compile-only
  validation harness.
- **N-3** `apps/shared/sframe-bridge/` — C++ subclass
  `LakkoFrameEncryptor` / `LakkoFrameDecryptor` of
  `webrtc::FrameEncryptorInterface` /
  `FrameDecryptorInterface` (the upstream M125
  shape). 4-test standalone round-trip suite proving
  AAD-binding, multi-sender cache, bad-seed rejection.
- **N-4** `apps/ios/Lakko/Calls/SFrameAttach.{h,mm}`
  + `Lakko-Bridging-Header.h` — Obj-C++ shim with
  boot-time `class_respondsToSelector` probe for
  `setFrameEncryptor:` / `setFrameDecryptor:`. Refuses
  to start the call when the probe goes negative.
  Swift-side `SFrameAvailability.shared` fail-closed
  gate. `cross-build-sframe-native-ios` justfile
  recipe lipo-fuses simulator slices.
- **N-5** `apps/android/lakko-sframe/` — Gradle AAR
  with Kotlin `LakkoFrameEncryptor` / `Decryptor`
  implementing `org.webrtc.FrameEncryptor` /
  `FrameDecryptor`. JNI bridge `libsframe_jni.so`
  built via `tools/build-sframe-jni.sh` against the
  cross-compiled Rust staticlib for each NDK ABI.
  Kotlin `SFrameAvailability` boot probe through
  `Class.forName + getMethod` against
  `org.webrtc.RtpSender.setFrameEncryptor`.
- **N-6** Production CallSession integration on both
  platforms — `setCallKey(seed, localLeaf)` injected
  after welcome-ferry / KP-forward dispatcher
  resolves; `attachSFrame` called between
  `addLocalAudioTrack` and SDP build so the
  encryptor is armed before libwebrtc's codec
  thread pulls bytes. Extended Rust E2E test
  `crates/lakko-sframe-native/tests/sfu_opacity_e2e.rs`
  (6 tests, all green) proving SFU opacity,
  AAD-tampering detection, wrong-key isolation,
  multi-party fan-in, cross-call isolation, counter
  monotonicity.

Validation chain (all green):

- `cargo test --workspace --all-targets` — 224+ tests
  pass including the new sfu_opacity_e2e suite.
- `xcodebuild -destination 'generic/platform=iOS
  Simulator' build` → BUILD SUCCEEDED. The full Swift
  / Obj-C++ / C++ / Rust path compiles and links
  cleanly with the lipo'd `liblakko_sframe_native.a`.
- `./gradlew :app:assembleDebug` → BUILD SUCCESSFUL.
  The full Kotlin / JNI / C++ / Rust path likewise.

**Deferred follow-ups (out of G-28 scope, tracked
separately)**:

- **N-{4,5}-validate** — host-side close-out DONE
  (May 2026). `apps/shared/sframe-bridge/test_bridge.cc`
  ships eight standalone C++17 assertions exercising
  every documented tampering mode of the SFrame
  native shim before either platform binding loads:
  single-sender round-trip, AAD-tamper, multi-sender
  per-leaf cache, bad-seed rejection, ciphertext-body
  tamper, counter monotonicity with out-of-order
  delivery, per-call key isolation (two distinct
  CallKey seeds cannot cross-decrypt), payload-size
  extremes (1B/80B/1500B/64KiB). Runner: `just
  test-sframe-bridge` locally and
  `.github/workflows/sframe-bridge.yml`
  (`workflow_dispatch` only, matrix `ubuntu-latest`
  + `macos-latest`) in CI. Pinned as audit-pack
  invariant **C-53** in
  [`docs/security/calls.md`](./calls.md).
- **N-{4,5}-validate-live** — dual-simulator /
  dual-emulator live-RTP gate (two iOS simulators
  + two Android emulators in CI running synthetic
  calls against the dockerised SFU from
  `ci/sfu-e2e/`, asserting tampered AAD →
  `kFailedToDecrypt` end-to-end). Lives in
  **G3-4-mobile** — the simulator / emulator matrix
  half of the parent **G3-4** sprint. Needs
  `macos-14` + KVM-enabled Linux runners we do not
  yet have budget for.

Build artefacts are already produced; the host-side
gate covers every code path below the platform shim,
and the dual-instance live-RTP gate is the only
remaining piece (waiting on CI-runner allocation).

---

**Historical context (preserved for audit trail)**:

The original G-28 investigation surfaced a structural
ceiling on both upstream WebRTC SDKs Lakko depends on:

- **Android `io.github.webrtc-sdk:android` 125.6422.07**
  exposes `org.webrtc.FrameEncryptor` and
  `org.webrtc.FrameDecryptor` only as Java interfaces
  with a single abstract method `long
  getNativeFrameEncryptor()` / `long
  getNativeFrameDecryptor()`. The actual `encrypt` /
  `decrypt` calls happen entirely inside the C++
  `webrtc::FrameEncryptorInterface` /
  `FrameDecryptorInterface` instance whose pointer
  that `long` returns. A pure-Kotlin override of an
  `encrypt(mediaType, ssrc, additionalData, frame)`
  method **does not exist on this SDK** — that shape
  matches the older `google-webrtc` artifact Lakko
  abandoned per G3-2 / ADR-0011's note that Google
  stopped publishing that artifact in 2021.
- **iOS stasel/WebRTC 125.0.0** — **G3-1-sframe-spike
  closed (May 2026)**: walked the
  `WebRTC.xcframework/ios-arm64/WebRTC.framework/Headers/`
  directory (92 public headers across iOS arm64, iOS
  simulator x86_64+arm64, macOS, mac-catalyst — every
  slice identical in this respect). The only public
  mention of `FrameEncryptor` / `FrameDecryptor` is the
  docstring on `RTCCryptoOptions.sframeRequireFrameEncryption`
  — a require-flag, **not** an attach-API. Neither
  `RTCRtpSender.h` nor `RTCRtpReceiver.h` exposes
  `frameEncryptor` / `frameDecryptor` /
  `frameTransformer` properties. The C++
  `webrtc::FrameEncryptorInterface` exists in upstream
  libwebrtc but stasel's Obj-C wrappers don't bridge
  it out. Same shape as Android.

**Path forward (Android, possibly iOS)**: a Rust crate
`lakko-sframe-jni` that compiles via `cxx-rs` to one
`.so` per ABI (arm64-v8a, x86_64). The `.so` exposes
a C++ subclass of `webrtc::FrameEncryptorInterface`
whose `Encrypt` / `Decrypt` thunks call back into
LakkoCore's `ParticipantKey` via the existing UniFFI
surface (a `long` pointer round-trips through the
Java `getNativeFrameEncryptor()` contract without
ever crossing back to the JVM mid-frame). This is
the canonical Insertable-Streams shape upstream
expects; it is also how Signal, Jitsi, and Matrix
do it. Estimated at ~2 sprints (cxx-rs setup +
libwebrtc header dep + Android NDK toolchain pin +
separate Gradle module for `.so` packaging).
Tracked as `G3-2-sframe-jni`.

**iOS path forward (post-spike)**: matching native-
shim shape under task **G3-1-sframe-native**. An
Objective-C++ (`.mm`) class subclasses
`webrtc::FrameEncryptorInterface` + `FrameDecryptorInterface`
from upstream libwebrtc's C++ headers (bundled
ourselves since stasel only ships Obj-C headers +
the linked binary; libwebrtc's BSD license permits
header redistribution). The shim is installed onto
`RTCRtpSender` / `RTCRtpReceiver` via the
non-public selector `setFrameEncryptor:` that the
upstream Obj-C wrapper carries but does not declare
in its public headers (the selector is hit by name
through `objc_msgSend` — same technique Signal-iOS
and Jitsi-Meet iOS use for the identical gap).
Inside `Encrypt` / `Decrypt` the shim calls into
LakkoCore's `ParticipantKey` via the existing
UniFFI surface. Estimated ~2 sprints in parallel
with G3-2-sframe-jni; the Rust side may share a
single crate (`lakko-sframe-native`) exposing a C
ABI both platforms wrap.

The native shim approach has a security caveat: the
non-public `setFrameEncryptor:` selector is not part
of stasel/WebRTC's stable API. An upstream rename or
removal would silently no-op the installation —
behaviour the audit pack must catch. Mitigation: a
boot-time `class_respondsToSelector` probe that
panics loud if the selector is missing, surfaced as
a "SFrame attach refused" log + the call MUST refuse
to start (no fallback to plain SRTP under any
circumstance, per security invariant #4 "no fallback
to non-E2EE").

**Security posture during the gap**: documented in
[`docs/security/calls.md`](./calls.md) under the
G3-1 / G3-2 partial-completion rows in
[`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md):
the iOS + Android call paths today ride **plain
SRTP** (per-hop encryption only). They are NOT
E2EE-complete. The wire still announces SFrame
support so the SFU still forwards opaque payload,
but the absence of an actual SFrame transformer at
the codec boundary means the SFU could in principle
decode the SRTP and see plaintext Opus/AV1. ROADMAP
explicitly states this. The fix is the JNI shim
above + the iOS spike + (subsequently) MLS-exporter
seed delivery from each platform's CallSession into
the freshly-created `ParticipantKey`.
+ replication via DeviceGroup are wired).

## AEAD (key, nonce)-reuse audit

<a id="g-33"></a>
### G-33: Channel publish-counter not persisted → AEAD nonce reuse across relaunch

**Status:** **CLOSED (2026-07)**

`lakko-channels` seals each `ChannelPost` with ChaCha20-Poly1305 under a key + nonce
HKDF-derived from `(content_seed, channel_id, epoch_id, sender_pubkey, message_counter)`
(`crates/lakko-channels/src/post.rs`), and the crate documents that `message_counter`
MUST be strictly monotone within `(channel_id, epoch_id, sender_pubkey)` and "lives in
client storage **between sessions**" (`post.rs:142-145`). The channel content seed is
**stable across an app relaunch/reload** (it re-derives from the persisted MLS
PublisherGroup exporter, and `epoch_id` is unchanged), so the counter is the ONLY input
that must be unique per post within an epoch. All three clients held the publish counter
**in memory only** (web `channels-state.ts` signal; iOS/Android `ChannelsUiState`
`counterByChannel`), restarting it at 0 on every relaunch + on `freshPublisherKeys`. A
relaunched publisher therefore re-issued an **identical `(key, nonce)`** on new
plaintexts within the same epoch — catastrophic ChaCha20 keystream reuse
(`P_a XOR P_b = C_a XOR C_b`) + Poly1305 forgery, exactly against the zero-trust server +
subscribers who hold the ciphertext.

**Fix (all 3 clients).** Persist the per-channel counter **at rest** and make it a
**forever-monotone floor** (safe: `epoch_id` is in the HKDF, so a monotone-across-epochs
counter is monotone within each epoch, and needs no per-epoch reset): web
`publish-counter-store.ts` (`localStorage`, non-secret operational metadata — the counter
rides in the clear in the post header), iOS `ChannelPublishCounterStore` (Keychain,
`AfterFirstUnlockThisDeviceOnly`), Android an `EncryptedSharedPreferences` row alongside
the publisher signing seed. **Load-on-init** restores the floor; **write-through** in
`nextCounter` persists `value+1` before returning `value` (a crash after a post can only
SKIP a counter, never re-issue one); `freshPublisherKeys` is **load-else-0** (seed 0 only
for a channel never published to, else keep the persisted floor — mirrors the G-29
publisher-seed reuse-else-generate); **no reset on epoch rotation**; and — the load-bearing
correction — `removeChannel` **KEEPS** the counter (un-publishing does NOT destroy the MLS
content seed, so a re-add of the same `channel_id` re-derives the same seed + epoch; the
counter must stay a floor so a re-add can never rewind it to 0 and re-collide, robust even
though the signing seed IS dropped on removeChannel). Verified: web vitest (19-case
`publish-counter-store.test.ts`, incl. the reload-regression), iOS `xcodebuild` +
Android `:app:testDebugUnitTest` (`ChannelPublishCounterTest`, incl. removeChannel-keeps).
Touches C-40..C-42 (channel publisher path); invariant #7 (the counter now persists in
hardware-backed storage on native).

**Residuals (accepted, narrow).** (a) **A counter may only be dropped together with the
publisher signing seed it was counting for.** Corrected 2026-10-01: this residual used to
say a counter reset followed by a same-epoch backup-restore of the PublisherGroup could
re-collide, which leaves out that `sender_pubkey` is an input to both the key and the
nonce. The sender signing key is random per device and channel (`SigningKey::generate`),
is never derived from the phrase or the account seed, and is carried by no backup,
pairing bundle or DeviceGroup sync, so once the seed is gone the next post signs under a
new key and a counter restarting from 0 cannot collide. Forget device and Delete account
therefore remove the account's counters together with its signing seeds on all three
clients (iOS since its per-account store, web and Android since 2026-10-01; web's counter
moved from a device-wide key to a per-account one). The rewind that stays dangerous is a
counter dropped while the same signing seed survives; Log out, account switch and
`removeChannel` keep the counter, and the residual turns live again only if the signing
seed ever becomes recoverable. (b) **Multi-device publisher** (one account publishing the same channel from two
devices) keeps independent counters and could collide — a pre-existing concern the crate's
per-client-storage contract explicitly scopes out; closing it needs a synced publisher
counter (a separate arc).

<a id="g-34"></a>
### G-34: System-wide AEAD (key, nonce)-reuse audit

**Status:** **T1 CLOSED + S1/S2 HARDENED + T2 (chat-ratchet nonce reuse) CLOSED (2026-07)**

Following the G-33 channel publish-counter finding (a real ChaCha20 `(key, nonce)` reuse
across relaunch), a **system-wide hunt for the same misuse class** swept every AEAD site:
calls / SFrame, backup / crypto / recovery, MLS / DeviceGroup / pairing, and
transport / push / attachments. The class is: a fixed AEAD key paired with a
non-unique nonce (a fixed / persisted / reset-to-zero counter, salt, or IV) on distinct
plaintexts, which yields keystream reuse (`P_a XOR P_b = C_a XOR C_b`) + Poly1305 forgery.
Outcomes below; the invariant index is unchanged by this entry (a free-form audit
record, adding no indexed C/I/T invariant — it stood at **8 I + 58 C + 15 T** when
this was written; the live counts live only in the gated Quick stats table in
[`invariant-index.md`](./invariant-index.md)).

**T1 — CLOSED (this pass).** Shadowsocks-2022 reused the AEAD counter between the
handshake **prelude** and the **body** stream: both began the per-direction AEAD nonce at
0, so the first body chunk reused `(subkey, nonce 0)` (and the second `nonce 1`) already
consumed by the sealed prelude — an on-path censor holding both ciphertexts recovers the
XOR of the two plaintexts and can forge under the reused `(key, nonce)`. Severity HIGH
**in the SS-2022 layer**, but this is a **transport / obfuscation** layer only: the inner
application traffic stays MLS-E2EE, so invariant #2 (server / on-path sees only
ciphertext + public artefacts) holds and there is **no message-plaintext exposure**; the
adversary is a censor, and the anti-censorship stack is built-but-not-deployed (Phase 5,
[ADR-0026](../adr/0026-anti-censorship-wiring.md)). **Fixed:** the body stream now
continues the nonce counter past the prelude (`SS_PRELUDE_NONCE_COUNT = 2`), matching the
SS-2022 single-continuous-counter wire model; both ends shift symmetrically (a closed
ecosystem, [ADR-0011](../adr/0011-rust-sfu.md) / no external SS peer). Touchpoints:
`crates/lakko-transport/src/{shadowsocks,aead_stream}.rs` + nonce-continuity tests
(`prelude_and_body_use_disjoint_nonces_same_subkey`,
`handshake_then_first_body_frame_does_not_reuse_prelude_nonce`, and the
`SS_PRELUDE_NONCE_COUNT` resume-layout pins).

**S1 / S2 — HARDENED (this pass, defense-in-depth, no active bug).** The SFrame
counter-0 / fresh-seed invariant (a new call seed must never repeat under a leaf's
counter, which restarts at 0 per fresh `ParticipantKey`) was **caller-enforced with no
core guardrail** and carried a misleading "harmless" doc on the rekey path. There was no
live reuse — the seed IS fresh per epoch (see V1 / V2 below) — but a future caller feeding
`rekey` the same seed twice, or re-deriving a `ParticipantKey` on an unchanged seed, would
silently reset a counter under an unchanged key. Now `CallKeyRing::rekey` +
`GroupCallSession::rotate_call_key` **no-op on an unchanged seed** (constant-time compare),
the counter-0 / fresh-seed invariant is documented on
`CallKey::{from_seed, participant_key*}`, and property tests capture it. Touchpoints:
`crates/lakko-calls/src/{sframe,call_key_ring,group_call}.rs`.

**V1 + V2 — VERIFIED FALSE-ALARM (recorded so they are not re-discovered).** V1: a
suspected SFrame call-media nonce reuse on a link-based / multi-joiner **crash-rejoin at a
fixed `call_id`** is **not a bug** — the SFrame seed is the MLS exporter output bound to a
**fresh-random group epoch secret** (`InitSecret::random` at `Group::create`), not to
`call_id`. Every host / join founds a group with independent key material, so the exporter
seed never repeats across a rejoin, and the call group is never persisted. V2: OpenMLS
provider persistence — the provider is an in-memory `MemoryStorage` snapshotted after every
mutation (the chat ratchet survives relaunch), but the **call group is never snapshotted**
(it is founded fresh each session), so the call path is outside it and cannot resurrect a
stale call-ratchet state.

**V1 was wrong for chat-group calls (corrected 2026-10-04, G-44).** The argument holds for
a link call, whose group is founded per call. A call started inside a chat group takes its
seed from the CHAT group's exporter for the call id, and joining or leaving a call advances
no epoch, so a rejoin in the same epoch derived the same seed and a counter at zero.

**T2 — chat-ratchet nonce reuse on crash — CLOSED (2026-07).** The residual first noted here
turned out REAL and CRITICAL (verified firsthand on all three clients): the chat send path
published the generation-N MLS application-message ciphertext to the DS **before** persisting
the post-`createApplicationMessage` (N+1) sender-ratchet snapshot, and the persist was
best-effort (swallowed failures). The OpenMLS provider is in-memory, so the snapshot is the
only durable ratchet copy — a crash (or a silently-failed persist) between publish and persist
reloaded the ratchet at generation N, and the next send re-consumed generation N: an identical
AEAD `(key, nonce)` on a different plaintext (`C1 XOR C2 = P1 XOR P2` **message-plaintext** leak
to the zero-trust server + Poly1305 tag forgery, RFC 9420 §9). Unlike T1 (obfuscation-layer),
this is the real E2EE message layer. **Fixed** by reordering every ratchet-advancing send to
`create -> persist(durable, awaited, FAIL-CLOSED) -> publish`: if the N+1 snapshot does not
durably land, the send is aborted and never publishes, so the gen-N ciphertext never leaves the
device and generation N stays safe to re-consume; a crash in the new window merely skips
generation N (receivers tolerate sender-ratchet gaps). Web `apps/web/src/chat/client.ts`
(`persistDurableOrThrow`), iOS `ChatClient`/`ChatRepository` + `IdentitySnapshotStore.putSnapshotDurable`
(persister wired once so every send is fail-closed by construction), Android `ChatClient`/`ChatRepository`
(required `persistRatchet` param gating all 7 send call sites). Strengthens invariant #4 (no
send without durable ratchet state). Residual: the commit paths (add/remove/role/approve) share
the same publish-before-persist ordering — a DISTINCT hazard (epoch desync / rolled-back-commit
resend, not same-generation nonce reuse) worth its own separate analysis, not part of this fix.

**Rest of the crypto surface: CLEAN.** Backup derives a fresh CSPRNG salt → Argon2id KEK
**per envelope** (v1 `envelope::seal`) so key + nonce are fresh; attachments use a fresh
key + nonce per blob; contacts sealed-box uses a fresh ephemeral per message; REALITY's
authenticator is an HMAC (no nonce); ECH is HPKE; ADR-0035 gives every `(leaf, stream)` a
disjoint `(key, nonce_salt)` space; the rekey-overlap ring only retains a **different**-seed
previous key; and all randomness is `OsRng`. Two latent footguns were **doc-hardened**
(INFO, no behavior change): `lakko-backup` `chain.rs::seal_v2` (DORMANT — superseded by
`head_frame` per [ADR-0037](../adr/0037-backup-head-rollback-commitment.md) — takes a
caller-supplied salt + nonce with no fresh-random wrapper; every caller is `#[cfg(test)]`)
now warns the caller MUST pass fresh CSPRNG salt AND nonce and points at v1
`envelope::seal`; `lakko-sframe-native` `lakko_sframe_wrap_frame` (stamps a **bare-leaf**
KID, not the ADR-0035 composite `(leaf << 8) | stream`; touches no key / nonce — the KID is
cosmetic / routing for pre-sealed bytes — and has **no production caller**, test-only) now
notes the keyed media path must use `seal_framed` / `open_framed`.

## Dependency advisories

The `cargo audit` baseline (RustSec advisory DB) surfaces
two open vulnerabilities + three unmaintained-crate
warnings in the workspace's transitive dependency tree.
None are in code we control; remediation depends on
upstream releases. Each entry below lists severity,
reachability analysis, and the upstream tracker the fix
is gated on.

<a id="g-23"></a>
### G-23: `libcrux-chacha20poly1305 0.0.7` — RUSTSEC-2026-0124 (HIGH 8.2)

**Status:** **CLOSED (fixed)**

> Potential Panic on Overlong Ciphertext Buffer.
> Upgrade to >= 0.0.8.

Vulnerable as a transitive dep:
`libcrux-chacha20poly1305 0.0.7 → libcrux-aead 0.0.7 →
hpke-rs-libcrux 0.6.1 → hpke-rs 0.6.1 →
openmls_rust_crypto 0.5.1 → lakko-mls` (HPKE seal/open inside
OpenMLS).

**Fix — a Cargo.toml-only vendored fork of `hpke-rs-libcrux`.**
The published `hpke-rs-libcrux 0.6.1` hard-pins
`libcrux-aead ^0.0.7` (which transitively pins the vulnerable
`libcrux-chacha20poly1305 0.0.7`), and a `[patch.crates-io]`
cannot relax a semver requirement. So we vendor `hpke-rs-libcrux`
under `crates/vendor/hpke-rs-libcrux` and patch it in via the root
`[patch.crates-io]`. The delta is **two version bumps in its
`Cargo.toml` only** — `libcrux-aead 0.0.7 → 0.0.8` (which pulls
the fixed `libcrux-chacha20poly1305 0.0.8`) and its own
`libcrux-traits 0.0.6 → 0.0.7` so the AEAD trait it imports
matches what `libcrux-aead 0.0.8` implements. **The `src/` is
byte-identical to upstream 0.6.1** (verified) — zero crypto-code
changes.

The earlier worry about an "irreconcilable `libcrux-traits`
0.0.6-vs-0.0.7 conflict" was wrong: `hpke-rs-libcrux` uses
`libcrux-traits` only for the AEAD trait, so `libcrux-ecdh` /
`libcrux-hkdf` staying on `libcrux-traits 0.0.6` while the AEAD
path uses `0.0.7` is fine — Cargo carries both versions and no
trait type crosses the boundary. The fix **compiles + the full
workspace checks clean + all `lakko-mls` tests pass**, and
`cargo audit` now reports the libcrux advisory gone (only the
accepted-risk `rsa` G-24 remains).

Regression-guarded by the existing `cargo audit` / `cargo deny`
CI gate: if the bump were reverted, `libcrux-chacha20poly1305
0.0.7` would return and the advisory check would fail.

(The vulnerable path was, in any case, not demonstrably reachable
— every byte reaching `libcrux-chacha20poly1305::decrypt` first
passes OpenMLS's length-bounded `MlsMessageIn` parser + the
HPKE-RS length checks, and the attacker-facing ingest is gated by
a rate limit + PoW + Privacy Pass — so this was a low-risk DoS at
worst even before the fix.)

<a id="g-24"></a>
### G-24: `rsa 0.9.10` — RUSTSEC-2023-0071 (MEDIUM 5.9) — Marvin Attack

**Status:** **CLOSED (Jun 2026)**

**Closed by dropping rsa, not accepting it.** Bumped `sqlx` 0.8 → 0.9
(released, `rust-version` 1.94 ≤ our pinned 1.95) across all 7 services and
omitted the now-feature-gated `rsa` feature (`sqlx-mysql/rsa`), so the crate is
no longer in the dependency tree at all — `cargo tree -i rsa` is empty and
`cargo deny check` passes with **no ignore** for RUSTSEC-2023-0071 (the
`deny.toml` entry was removed). The only 0.9 breaking change for our usage was
the SQL-injection guard (`SqlSafeStr`): the 4 audited dynamic-SQL sites in
`services/ds/src/journal.rs` (table/column from a fixed `Queue` enum, never user
input) now wrap the string in `sqlx::AssertSqlSafe`. Workspace `cargo check` +
`cargo deny` + the `ds` tests are green. The original analysis below is retained
for history.

---

> Potential key recovery through timing sidechannels.
> No fixed upgrade is available.

Transitive dependency: `rsa 0.9.10 → sqlx-mysql 0.8.6 →
sqlx → all `services/*`.

**Why this is structural, not negligence.** The `rsa`
crate is the only pure-Rust RSA implementation that
compiles on every target sqlx supports; the Marvin-attack
mitigation requires constant-time arithmetic that the
crate maintainers explicitly call out as "we will not
land this in the current codebase." The upstream
position is "use a different RSA library if you need
side-channel resistance" — `sqlx-mysql` would have to
swap implementations, which it has not.

**Reachability.** `rsa` is reached **only** via MySQL
native-auth handshake when the server's
`mysql_native_password` authentication method requires
RSA-OAEP padding to encrypt the client password (server-
side `caching_sha2_password` cache miss path). Lakko
configures MariaDB with `caching_sha2_password` ONLY,
and the warm-cache hit path uses SHA-2 without RSA. The
RSA path is reached only on first-time connect from a
client whose password hasn't been cached server-side.
**The application never sees user passwords directly**:
authentication runs OPAQUE-PAKE (`services/auth`), which
the database connection layer is downstream of — the
MySQL connection password is the **service's** own DB
password (configured in `/etc/lakko/lakko.env`), not a
user's. A successful timing attack on this RSA exchange
would leak the service's MySQL connection password to an
attacker who already has network access to MySQL itself
— at which point the attacker has direct DB access and
the RSA leak is the wrong attack to spend a Marvin
side-channel on.

**Mitigation**: production MariaDB binds only to the
internal loopback / VPC interface; only the Lakko
services reach it. An attacker who can mount a timing
side-channel on this internal hop has already broken
the perimeter.

**Status**: accepted risk pending upstream `sqlx-mysql`
swapping to a constant-time RSA crate (no tracker —
upstream position is "no fix coming"). Re-audit if MySQL
auth surface changes.

**Corrected + re-verified (Jun 2026).** An earlier note here
claimed this was "not fixable upstream" — that was wrong (it
parroted a stale `deny.toml` comment). The real status:
- `cargo update --dry-run` shows 0 **semver-compatible** updates
  — `rsa` 0.9.10 + `sqlx` 0.8.6 are the latest 0.x releases —
  but that only covers in-range bumps.
- On `sqlx-mysql` **0.8.x** the `rsa` crate cannot be removed:
  `optional = true` in the manifest, yet `connection/auth.rs`
  does `use rsa::{…}` UNCONDITIONALLY (no `#[cfg(feature="rsa")]`),
  so dropping the feature is a compile error.
- **`sqlx` 0.9.0 is released** (the stale comment said sqlx was
  still "waiting on rsa 0.10.0") and **feature-gates rsa**
  (`sqlx-mysql/rsa = [dep:rand, dep:rsa]`). The advisory itself
  has `patched = []` — no fixed rsa exists — so the fix is to
  **drop** rsa, not upgrade it: bump 0.8 → 0.9 and omit the `rsa`
  feature, and the crate leaves the tree entirely (our MariaDB is
  localhost-only over a Unix socket / TLS, so the
  caching_sha2_password RSA path is never exercised).
- **Status: a real, concrete fix exists (sqlx 0.9 + no rsa
  feature) — deferred only because the 0.8 → 0.9 bump is a
  breaking, multi-service migration.** Documented accept in
  `deny.toml` stands in the meantime; residual is near-zero per
  the reachability analysis above.

<a id="g-25"></a>
### G-25: Unmaintained-crate + advisory warnings

**Status:** **all CLOSED (2026-07): the three unmaintained warnings + two 2026 transitive advisories (anyhow, crossbeam-epoch); `cargo deny check advisories` is clean**

The `cargo audit` baseline flagged three unmaintained-crate
advisories. None are vulnerabilities — the maintainers wound
down support, but the code works. Two were transitive through
`uniffi 0.28.x` and **closed** by the `uniffi 0.28 → 0.31` bump
(the same "major upstream bump drops the dep" remediation that
closed G-24's `rsa` via sqlx 0.9). The third (`instant`) reaches
us only on the wasm target via `openmls`'s `js` feature and is a
non-CVE unmaintained advisory. It has a **self-serviceable fix
that does not wait on openmls** — a `[patch.crates-io]` shim
re-exporting the maintained `web-time` in place of
`fluvio-wasm-timer` (detailed in the `instant` bullet below) —
but the maintainer deferred it (2026-07) as supply-chain surgery
disproportionate to a cosmetic-audit win; the accept posture
stands until a clean `cargo audit` is required.

- **`bincode 1.3.3`** (RUSTSEC-2025-0141) — **CLOSED
  (Jun 2026).** Was under `uniffi_macros 0.28.3 →
  uniffi 0.28.3` (build-time macro-output serialisation).
  `uniffi_macros 0.31.2` no longer depends on `bincode`
  (verified: `uniffi_macros@0.28.3` declared `bincode ^1.3
  kind=normal`; `@0.31.2` declares neither). After bumping
  `crates/lakko-ffi-uniffi` to `uniffi = "0.31"`,
  `cargo update -p uniffi` reported `Removing bincode v1.3.3`
  and `cargo tree -i bincode` is now empty.

- **`paste 1.0.15`** (RUSTSEC-2024-0436) — **CLOSED
  (Jun 2026).** Was under `uniffi_core 0.28.3` +
  `uniffi_bindgen 0.28.3` (compile-time macro helper).
  Both dropped `paste` at 0.31.2 (replaced by the maintained
  `pastey` fork). `cargo update -p uniffi` reported
  `Removing paste v1.0.15` and `cargo tree -i paste` is now
  empty.

- **`instant 0.1.13`** (RUSTSEC-2024-0384) — **CLOSED
  (2026-07) via a `web-time` shim.** Was under
  `parking_lot_core 0.8.6 → parking_lot 0.11.2 →
  fluvio-wasm-timer 0.2.5 → openmls 0.8.1`. A wasm-only
  timer abstraction (an **unmaintained** advisory, not a
  CVE). The self-serviceable fix documented below was
  **applied**: a vendored `crates/vendor/fluvio-wasm-timer`
  shim (`pub use web_time::{SystemTime, UNIX_EPOCH};`, AGPL,
  excluded from the workspace) wired in via `[patch.crates-io]`
  REPLACES the real crate, so `instant` leaves the graph
  entirely — `cargo tree -i instant --target all` reports "did
  not match any packages" and `cargo deny check advisories` no
  longer lists it. openmls builds + tests green against the shim
  on host + wasm (the wasm build is the one that actually uses
  the shim's `SystemTime` / `UNIX_EPOCH`).

  **What functionality pulls it (verified 2026-07 against
  the openmls 0.8.1 source):** `fluvio-wasm-timer` is an
  optional openmls dependency gated behind openmls' `js`
  feature, which Lakko enables in
  [`crates/lakko-ffi-wasm/Cargo.toml`](../../crates/lakko-ffi-wasm/Cargo.toml)
  (`openmls = { features = ["js"] }`). The `js` feature also
  carries `getrandom/wasm_js` — mandatory (no RNG, hence no
  MLS, on wasm without it) — so the feature itself cannot be
  dropped. openmls `use`s the timer in exactly one place,
  `src/key_packages/lifetime.rs`, under
  `#[cfg(target_arch = "wasm32")]`, to read `SystemTime::now()`
  for **KeyPackage lifetime (expiry) validation** — checking
  a KeyPackage's `not_before` / `not_after` window when a
  member is added or a group is joined on the web client. On
  wasm `std::time::SystemTime::now()` panics (no OS clock), so
  openmls substitutes `fluvio-wasm-timer`'s `SystemTime`
  (backed by JS `Date.now()`). The functionality is genuine
  and security-relevant (an expired KeyPackage must be
  rejected), so the code path cannot simply be removed.

  **A self-serviceable fix exists (deferred, not blocked on
  openmls):** openmls consumes only `fluvio_wasm_timer::{SystemTime,
  UNIX_EPOCH}`, and the maintained replacement `web-time`
  exposes the same std-compatible surface. A `[patch.crates-io]`
  entry pointing `fluvio-wasm-timer` at a tiny vendored shim
  crate whose body is `pub use web_time::{SystemTime, UNIX_EPOCH};`
  would satisfy openmls with **zero openmls source change** and
  drop `instant` from the graph entirely (`cargo tree -i instant`
  empty, incl. under wasm) — the same vendored-patch idiom Lakko
  already uses for `webauthn-rs-core` and `hpke-rs-libcrux`, and
  a net supply-chain improvement (an unmaintained crate replaced
  by a maintained one). **Applied 2026-07** (the maintainer called
  for the clean-audit posture): the shim landed exactly as described
  and `instant` is gone. **Two 2026 transitive advisories surfaced in
  the same audit run and were fixed by patch bumps** (unrelated to the
  shim — the RustSec DB had published them since the prior audit):
  `anyhow 1.0.102 → 1.0.103` (RUSTSEC-2026-0190, an `Error::downcast_mut`
  unsoundness) and `crossbeam-epoch 0.9.18 → 0.9.20` (RUSTSEC-2026-0204,
  an invalid-pointer-deref in a `fmt::Pointer` impl, via `rayon`), both
  `cargo update -p` lock bumps with no source change. `cargo deny check
  advisories` now reports **`advisories ok`**.

  The alternative upstream fix — openmls' planned 0.9 (which
  swaps `fluvio-wasm-timer` for `web-time` itself) — is
  unreleased and its timeline / scope are not guaranteed; when it
  does land, the bump is an MLS-core change requiring an ADR +
  property tests regardless. The shim above does **not** wait on
  it.

**The uniffi 0.28 → 0.31 bump cost zero source changes** —
`lakko-ffi-uniffi` uses proc-macro mode (`setup_scaffolding!`)
with a crate-local bindgen (`uniffi_bindgen_main()`), so the
runtime and generator move in lock-step. Verified end-to-end
firsthand (2026-06-23): `cargo build -p lakko-ffi-uniffi` +
`cargo test -p lakko-ffi-uniffi` (158 passed) + `cargo clippy`
clean; `just regen-bindings` (Swift + Kotlin) succeeds; the iOS
client (`xcodebuild` BUILD SUCCEEDED after `just build-ios`) and
the Android client (`:app:compileDebugKotlin` BUILD SUCCESSFUL)
both compile against the uniffi-0.31-generated bindings with no
hand-written-code changes; `just audit` reports `advisories ok`.

**Status**: bincode + paste closed; instant accepted upstream
debt (wasm-only, openmls-pinned). CI warns (not fails) on
unmaintained-crate advisories so they remain visible without
blocking releases.

**Cumulative posture** (updated 2026-06-23): zero
vulnerabilities in Lakko-authored code. The transitive-dep
advisories have since been remediated as the upstream chain
caught up — `rsa` (RUSTSEC-2023-0071, G-24) closed by sqlx
0.9, `libcrux-chacha20poly1305` (RUSTSEC-2026-0124, G-23)
closed by the vendored hpke-rs-libcrux fork, and `bincode` +
`paste` (G-25) closed by the uniffi 0.31 bump. The only
remaining unmaintained-crate warning is `instant`
(RUSTSEC-2024-0384, G-25) — wasm-only, pending an openmls
release that drops `fluvio-wasm-timer`. The `cargo audit`
baseline below is captured as of `2026-05-28` (re-run delta
vs `2026-05-27`: **zero change** — same 2 vulnerabilities,
same 3 unmaintained warnings, no new findings, no advisories
resolved upstream at that time. Post-G5-6-wireup
confirmation: the
`hpke 0.13` + `rand_core 0.9` deps added by ADR-0012
brought in their pure-Rust RustCrypto dependencies
[`x25519-dalek`, `aes-gcm`, `chacha20poly1305`,
`hkdf`, `hmac`, `sha2`] without introducing any new
advisory) and is re-run on every CI build; new
findings either get a G-entry of their own or are
remediated as the upstream chain catches up.

`cargo deny check` is the second tier of the supply-
chain gate (licence allowlist, advisory deduplication,
unmaintained-crate policy on top of `cargo audit`). The
binary is not currently installed on this dev host; the
`just audit` recipe invokes it during CI runs. A follow-
up sprint stands up `cargo deny` locally on the
maintainer's box so the audit pack can mirror its CI
output here. **Corrected 2026-09-22:** `cargo deny` does
run on the maintainer's box - the G-41 analysis below was
produced by `just audit` locally.

## Client concurrency

Chat-client locking + ordering gaps. Both entries below concern the CLIENT side of
the MLS commit / poll paths, not a server or a dependency.

**Filing note.** Everything from G-27 to G-36 still sits under the preceding
`## Dependency advisories` heading, which describes none of them — a structural
drift from a run of arcs that appended entries without opening a section. Only
this section was opened; re-filing the rest is a standing cleanup, deliberately
not folded into this pass because it would move a lot of unrelated prose. The
reviewer-facing categorisation in
[`invariant-index.md`](./invariant-index.md) is correct for all of them.

<a id="g-38"></a>
### G-38: Two membership commits in flight to the DS at once

**Status:** Inverted journal order, device-ahead. **serialized on web + iOS + Android and the local poll-vs-commit interleave closed on all three (2026-08); the remote-commit epoch fork is CLOSED for the member membership paths by ADR-0045 and, since S7 (`576ea14`), CLOSED for the external-join path too — the last direct-commit route in a role-table group is gone**

> **S7 closed the last route (2026-09-01, `576ea14`).** An approval is now an `Add`
> proposal the approving admin **authors itself**, carrying the joiner's retained
> KeyPackage and swept like any other, so two admins approving two joiners against one
> epoch compose instead of diverging — measured before the fix as three devices all
> reporting epoch 4 with three different rosters. ADR-0045's own decision 4 prescribed a
> different design (store the stranger's proposal, mark it approved, admit it with a
> second sweep arm) and it **did not survive contact**; the three reasons are in
> [ADR-0045 § Why decision 4's design does not survive](../adr/0045-remote-commit-epoch-fork.md#why-decision-4s-design-does-not-survive),
> and the sharpest is that merging the stores merges the quotas, which would hand any
> `/g/` link-holder the membership-freeze primitive C-66 had just closed. Two residuals
> S7 leaves, both stated rather than buried: a sweep that **excludes** an
> approval-derived `Add` (an expired KeyPackage being the one cause needing no adversary)
> gives the joiner **no signal at all**, indistinguishable from being ignored; and
> nothing bounds how long a request sits in the quarantine when no committer is running —
> the cap catches it eventually, but as a refusal to the *next* proposer rather than as a
> signal to anyone.
>
> **The closure had seven holes of its own, all now closed, and that is the honest
> headline of this gap.** The `lakko-security` review ADR-0045 names as a precondition
> for its own Accepted status finally ran and returned **REQUEST CHANGES**: three HIGH
> findings plus a confirmed impersonation, all recorded in
> [ADR-0045 § What the security review found](../adr/0045-remote-commit-epoch-fork.md#what-the-security-review-found)
> and indexed as **C-66**, **C-67** and **C-68**. What matters for this gap's severity
> is that **none of the seven needed an attacker, and four needed nobody at all** — a
> network blip, a stale roster, two admins doing the ordinary thing, or simply enough
> time passing. A mechanism that closes a fork by splitting an atomic operation buys
> that closure with a larger surface of *partial* states, and this is what that cost
> looked like when it was finally counted.
>
> **Residuals, stated rather than buried:**
>
> - **The build-condition enumeration is a mirror of a rule set Lakko does not own.**
>   Fifteen conditions are enumerated; an openmls upgrade can add a sixteenth. That is
>   precisely why C-66 backs it with an exclusion search that needs no knowledge of the
>   rules — but the search is a strong general answer, not a proof, and a build failure
>   it cannot route around would still wedge a group.
> - **Two build conditions are left unenforced**, both deliberately: whether every
>   *existing* member's capabilities accept a joiner's credential type (a property of
>   the group, and openmls exposes no member capabilities), and the commit path's own
>   encryption-key uniqueness (generated inside `build`, invisible to a proposal filter).
> - **A proposal dropped by the exclusion search reports no reason.** `CreateCommitError`
>   carries no `ProposalRef`, leaf index or signature key on any variant, so a client can
>   tell an author *your request was not carried* and never why. Naming a reason would
>   mean inventing one.
> - **A latent `SelfRemove` wedge is armed by a wire-format-policy change.**
>   `ProposalType::SelfRemove` is not a default type and no Lakko leaf advertises it, so
>   a stored one would fail every build with `UnsupportedProposalType`. It is
>   structurally unreachable today in both directions — openmls refuses to emit one under
>   the pure-ciphertext policy, and an inbound `PublicMessage` is refused before the
>   receipt gate — and the existing named test that pins that refusal is what would fail
>   on the day someone changes the policy.
> - **One degraded corner can duplicate a timeline row** (C-67): a persist fails, the
>   debounced message-log write lands with the old cursor and the new rows, then the
>   process dies. Cosmetic, and strictly better than the permanent strand it replaces.
> - **The external crypto review ADR-0045 also requires has still not happened**, and
>   since [ADR-0046](../adr/0046-external-crypto-review.md) it is tracked once, as
>   **G-39**, rather than as a per-ADR blocker — a **release gate**: no build reaches
>   users while it is open. The `lakko-security` pass was one of the two preconditions
>   and is now done; the other is outstanding, and the surface it would review grew
>   again with these seven fixes.

**Get the mechanism right, because the obvious statement of it is wrong.** The hazard
is **not** "two commits built on the same epoch". On all three clients the
create-commit region has no suspension point, so a second operation correctly reads
N+1 after the first has eager-merged. The hazard is that **both commits end up in
flight to the DS simultaneously**, and the DS assigns journal order by **arrival**,
not by which call started first. Land them inverted and peers still at N drop the
N+1 → N+2 commit as wrong-epoch, apply the N → N+1 one, and settle at N+1 while the
committing device sits at N+2 — the **device-ahead** outcome
[ADR-0040](../adr/0040-durable-pending-commit-outbox.md) exists to prevent.

**The pending-commit outbox cannot recover it.** Both publishes *succeeded*, so both
entries are acked and the restart sweep has nothing to re-publish. ADR-0040's
durability machinery is aimed at the crash window between publish and persist; this
is a different route to the same end state, and it walks straight past it.

**Fixed by a per-group serializing lock spanning create-through-publish**, on all
three clients:

- **Android** — the two-tier shape described in G-37: the per-group `Mutex` spans the
  whole operation, so commit A is on the wire before commit B is even created.
- **iOS** — `ChatRepository.withCommitLock(groupIdHex:)`, a stored per-group task
  chain modelled on `PushGroupRouting.routingChain` (ADR-0044 C-62), for the same
  reason: a read-decide-write that spans an `await` needs a lock, not actor
  isolation. `@MainActor` makes each statement atomic but releases isolation at every
  suspension, and the region contains one (`dsClient.publishGroup`).
- **web** — `withGroupCommitLock` in `apps/web/src/contacts/client.ts`, a
  module-level per-group queue. Module-level is load-bearing: a `ContactsClient` is
  constructed per route component while they all drive the same canonical wasm
  `Identity`, so an instance field would serialise nothing that matters.

All three are **serializing, never coalescing** — a successor waits and then runs in
full against freshly-read state. Folding was `lakko-security` F1 in the ADR-0043
review; here it would be worse, because two membership operations answer two
*different* intents (add Alice, remove Bob) and a fold would silently drop one. The
lock spans the **create**, not just the publish — acquiring it afterwards would fix
nothing, since the eager merge is exactly what goes stale.

**Reachability, per platform, as established.**

- **web — reachable in a single tab, proven by execution.** The `busy` signal in
  `routes/Chat.tsx` is one shared boolean with **eight** independent owners
  (`refresh`, `onAddMember`, `onRemoveMember`, `onToggleAdmin`, `onApproveJoin`,
  `onRejectJoin`, `onCreateInviteLink`, `onRevokeInviteLink`), and `refresh()` is
  launched fire-and-forget (`void refresh()`) at the tail of each commit handler, so
  it routinely outlives the handler that started it and clears the flag out from
  under an unrelated commit still in flight. `commit-lock.test.ts` reproduces that
  ownership pattern directly rather than asserting it in prose, and the suite's own
  header records which cases **discriminate**: neutering the lock to a pass-through
  fails the same-group ordering test with `expected 7n to be 6n` (two eager merges
  during one publish), and re-keying it to a single global name times out the
  different-groups test.
- **iOS — reachable; a code-structure argument, not an observed run.** A latch does
  exist (`ChatViewModel.membershipBusy`) but the view model is constructed **inline**
  in the SwiftUI body (`LakkoApp.swift`) and held by `@ObservedObject`, so a body
  re-evaluation mints a fresh instance with the latch cleared. The repository methods
  are also `public` and carry no invariant of their own.
- **Android — not reachable before the change**: the repository-wide mutex already
  prevented it, and the G-37 narrowing preserves the same-group guarantee by design.

**Residuals — read residual 1 first. It was the largest unfixed item here and it is
now CLOSED for every membership path: for add / remove / role-change by ADR-0045
(2026-08-25) and for external joins by its S7 (2026-08-31, `576ea14`). What is left
under residual 1 is a live multi-device proof, not a mechanism.**

1. **The remote-commit fork. CLOSED for every membership path — the member paths on
   2026-08-25 ([ADR-0045](../adr/0045-remote-commit-epoch-fork.md)) and the
   external-join path on 2026-08-31 (S7, `576ea14`).** The hazard, unchanged: a poll applying a **remote** commit while a local
   one is mid-publish is a genuine MLS commit race — the peer built its commit
   against epoch N independently, so whichever commit loses the DS journal ordering
   is rejected by everyone who applied the winner, while its own author has already
   eager-merged it locally. Local serialization is powerless here **by
   construction**: the second committer is a different device. The two ways out were
   a **DS-side epoch gate** or **proposal-based commits**; ADR-0045 chose the second,
   and the reasons the first was rejected are recorded there (a DS that cannot verify
   a commit must accept well-formed garbage and bump its counter on it, which hands
   anyone who has ever held a `/g/` invite link a permanent group-freeze primitive).

   **What is closed.** In a group with an ADR-0022 role table, **add, remove and
   role-change no longer commit on any client**: a member emits an MLS **Proposal**,
   which advances no epoch, and exactly one elected owner-or-admin sweeps the pending
   set into a single commit. A proposal that loses a race is not a fork and needs no
   repair — the retry moved from the expensive artifact to the cheap one. The
   committer is elected by a pure total function of the authenticated `0xFF02` role
   table (`committer_rank`), so there is no election message and nothing to
   split-brain on, and the sweep **stages** its commit rather than eager-merging: it
   stands down on any unapplied journal row, publishes, reads the verdict by applying
   the rows below its own commit, and only then merges or abandons. Two candidates
   that both act therefore **do not fork** — the loser abandons. Built on the core
   (`9a60ab3`, `e780952`, `fe3de6c`, `a90fa90`), both FFI surfaces (`c5c0788`,
   `6c77cd6`, `94da492`) and all three clients (`1de5fe7`, `02f0037`, `1f230ab`).
   Pinned as **C-64**; the ownership-transfer safety rule the same arc produced is
   **C-63**.

   ~~**What is NOT closed.** `Group::approve_external_join` still calls `add_member`
   and **eager-merges** … That convergence is ADR-0045's S7 and it is unbuilt.~~
   **S7 landed on 2026-08-31 (`576ea14`) and this paragraph is superseded.**
   `approve_external_join` publishes an `Add` **proposal the approving admin authors
   itself**, carrying the joiner's retained KeyPackage, so two admins approving two
   different joiners against one epoch now compose into a single sweep instead of
   forking. The user-visible dead end went with it: `refuse_if_proposals_pending` no
   longer reaches the approve, because there is no direct commit left for it to
   guard. The one direct arm that survives is `pub(crate)
   approve_external_join_directly`, whose single caller is
   `PublisherGroup::approve_external_join` and which **refuses** a group carrying a
   role table — so it exists exactly where the proposal path cannot work (a
   `PublisherGroup` seeds no role table, hence has no committer) and nowhere else.

   **Three residuals of the closure itself**, all recorded in the ADR rather than
   here — the first two predate S7: a chat message can still straddle an epoch when the commit
   that changes it is a **remote** one (the clients serialized seal-and-publish, which
   closes the local half only; closing the rest needs `max_past_epochs` above zero in
   `lakko-mls`, an invariant-#3 change with its own forward-secrecy cost), and
   `PublisherGroup::rotate` / `self_update` remain a fifth epoch-advancing path
   ADR-0045 does not scope. S7 adds a third: a sweep can **drop an authorised
   approval** (fifteen enumerated `CommitBuilder::build` conditions plus the
   exclusion pass, the commonest needing no adversary being a KeyPackage expiring
   while a request waits under decision 2's deliberate absence of a deadline), and
   the **joiner cannot be told** — the group journal is unauthenticated and its DS
   pseudonym *is* the `group_id`, so a "your request was not carried" envelope would
   be a forgeable denial primitive any `/g/` link-holder could post, and an epoch
   advance without a rendezvous is byte-identical to nobody having approved yet. The
   **approver** does learn, via the `Not applied (n)` surface (`46c7aa4`); the joiner
   gets honest wait copy and a user-driven repair.

   The sub-case that was platform-dependent is **closed on all three**: the poll apply
   takes the same per-group lock as the commits, with the network fetch outside it and
   the MLS apply plus the cursor advance inside, so the purely **local**
   poll-vs-commit interleave can no longer occur.
2. **web two-tab is NOT fixable by a lock and was deliberately not patched.** Each tab
   holds an independent in-memory wasm provider, and the snapshot is written whole to
   a row keyed on `accountId` alone (`chat/mls-store.ts`). Tab B, even after acquiring
   a cross-tab lock, still builds on a stale epoch and clobbers tab A. Two tabs on one
   account are already broken independently of commits — two ordinary messages clobber
   each other the same way — so a cross-tab lock here would buy nothing and imply a
   safety that does not exist.
3. **web `CoPublishersPanel` (`routes/Channels.tsx`) — CLOSED (2026-08).** It was
   unguarded: a 5-second
   `setInterval` calls `refreshPending()` → `pollCoPublisherJournal` →
   `processIncomingVerifyingRoles` **without consulting the panel's `busy()` signal**,
   so it runs concurrently with `approveCoPublisher` — which is itself an
   `approveExternalJoin` create-commit on the `PublisherGroup`. That path is not
   covered by the commit lock, which at the time lived in `contacts/client.ts` and
   guarded the four `ContactsClient` paths only. The lock has since been extracted to
   `apps/web/src/mls/group-lock.ts` as `withGroupMlsLock` — one queue for one
   resource, importable without dragging discovery and transparency-verify into the
   chat bundle — and both the co-publisher poll and the approve path now take it.
4. **The web lock is per-group but the durable unit underneath is per-account.**
   `persistSnapshotWithCommit` writes `Identity.snapshotStorage()` — the whole
   provider — and the F1 abort path calls `restoreStorage` on that whole provider, so
   concurrent commits on two *different* groups would interact through the shared
   snapshot. Not reachable today (only the chat view commits on an existing group, and
   the router mounts one at a time), which is why the key stays per-group; the moment
   a second commit-capable view can be mounted the key must widen to the account. This
   is the same whole-provider granularity G-37's residual names on Android.
5. **The lock is held across an unbounded network call on web.** `DsClient.publishGroup`
   is a bare `fetch()` with no `AbortSignal`, so a stalled DS publish wedges **that
   group's** commit chain for as long as the browser keeps the request alive. iOS
   relies on `URLSession`'s implicit default request timeout rather than an explicit
   budget (only the long-poll fetch sets `timeoutInterval`). This is G-37's class
   reappearing at a much smaller blast radius — per group, not per repository — and it
   is a deliberate trade on iOS and web: releasing the lock while a publish's fate is
   unknown re-opens exactly the hazard the lock closes, so the bound belongs on the
   call, not on the section. Android's transport-level budget (G-37 part 2) is the
   worked example.

**Invariants — of the SERIALIZATION fix.** No `lakko-mls` change on any client —
orchestration over the audited create-commit and publish paths, so invariant #3 is
untouched. Invariant #4 is **strengthened**: no group-epoch advance reaches the DS
while another advance on the same group is still in flight. Invariants #1 / #2 are
unaffected (the lock changes ordering, not what the server sees). That fix added no
indexed `C-` / `I-` / `T-` invariant.

**Invariants — of the ADR-0045 closure of residual 1**, which is a different change
and a much larger one. It **does** touch `crates/lakko-mls` (invariant **#3**): a new
per-proposal authorisation rule, a commit-level never-orphan-the-owner rule, a new
stage / merge / abandon commit lifecycle, and a park on the send path. ~~**The external
crypto review that ADR requires and a `lakko-security` pass have both still not
happened on any of it**~~ — **the `lakko-security` pass RAN on 2026-08-29**, returned
REQUEST CHANGES on seven findings (three HIGH plus a confirmed impersonation) and all
seven are closed; the **external crypto review has not run** and is now tracked once,
as **[G-39](#g-39-the-external-cryptographic-review-of-the-mls-state-machine-has-not-happened--open-and-it-is-a-release-gate)**,
per [ADR-0046](../adr/0046-external-crypto-review.md). It remains the largest open
item on the arc, and it grew rather than shrank once three client implementations of
the same disciplines landed on top. ~~Two indexed invariants were minted: **C-63** and
**C-64**~~ — **seven**: C-63 and C-64 from the build, C-65 from the concurrent
role-change wedge, C-66 / C-67 / C-68 from the security review, and C-69 from S7. See
the Quick stats table in [`invariant-index.md`](./invariant-index.md) for the current
counts.

**Verified firsthand by the maintainer against the tree at the time of the
serialization fix:** Android `:app:compileDebugKotlin` BUILD SUCCESSFUL and
`testDebugUnitTest --rerun-tasks` 515 tests / 0 failures; web `tsc --noEmit` clean
and vitest 583; iOS `xcodebuild` BUILD SUCCEEDED with LakkoTests 342/342 excluding
the two dockerised-SFU cases; `just spec-check` OK. **The ADR-0045 client sprints
record their own gates** (`1de5fe7` / `02f0037` / `000d9f8` / `1f230ab`): web vitest
706, iOS 432 tests, Android 598 tests, all with zero failures. The residuals above
are **not** covered by any of those gates — what remains under residual 1 (a live
multi-device proof of the whole propose-sweep-adjudicate loop) in particular needs a
live multi-device environment. This sentence previously read "the remaining half of
residual 1", meaning the unbuilt external-join convergence; that half is built.
