<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0077: Work started for one account never acts for another; a wiped account's stores take no late write; a refused persist publishes nothing

- **Status:** Accepted (2026-10-14). Decisions 2 and 3 are the maintainer's
  (2026-10-13); the rest is built and host-tested, not walked live. Built on
  web, iOS and Android in `180602c1`, `67a81513`, `1fe2c97d`, `87c618fc` /
  `983393a4`, `16d6a27f`, `c22ad3e3` and `3af913ad`. Nothing was run on a
  device, an emulator or in a browser.
- **Date:** 2026-10-14
- **Related:** [ADR-0017](0017-phrase-seeded-identity.md) (the lost-everything
  fallback `mintFreshIdentity`, which decision 3 governs),
  [ADR-0040](0040-durable-pending-commit-outbox.md) (persist before publish for
  commits, which decision 5 extends to every publish),
  [ADR-0073](0073-account-key-reset.md) (since the account-recovery merge
  `1ae14935`, `mintFreshIdentity` is the attested key reset of section 2.3),
  [ADR-0075](0075-recovery-email.md) (new words are shown only to a
  phrase-protected account).
- **Indexed as:** C-72, C-73, C-74 in
  [`invariant-index.md`](../security/invariant-index.md); open residuals
  G-47, G-48, G-49 in [`known-gaps.md`](../security/known-gaps.md) (and the
  related channel findings G-50, G-51).

## Context

On all three clients the objects that hold an account's state live longer than
the account's session. The web providers under `<AuthProvider>` live for the
page; the iOS repositories and sync runtime and the Android repositories live
for the process. A sign-out, a Forget device or another account's sign-in tells
them the session ended; it does not stop the work they already started.
Nothing cancels a promise; an unstructured Swift `Task` that is cancelled still
runs the code after an `await` that returned; a coroutine launched in a
process-lived scope outlives the session. So work started for account A writes,
after its wait, into whatever the owner holds by then: nothing, the next
account B's state, or a store of A that a wipe has just deleted.

The defects found by reading (the commit messages list them; none was observed
on a device):

- **Calls.** A call left while devices or relay credentials were fetched went
  on to build a peer connection, open the microphone and the camera and send
  an offer, with no call on screen; an abandoned start could hang up or end a
  newer call; a web group-call start the person left still minted a token,
  connected, and left its `GroupCallStart` in the chat.
- **Account crossings.** iOS logout did not clear the chat repository, so the
  receive loop went on polling, decrypting, sweeping and ringing for the
  signed-out account. A late push unregister could remove the next sign-in's
  registration (iOS, including VoIP). An Android reach reconcile in flight at
  sign-out could save a near-empty document under the departed account's key.
  The Android own-echo registry survived a wipe, so the next account in the
  same group dropped rows the first one had published as its own echoes.
  Android built a second set of repositories on every Activity recreation
  while retained view models kept the first: two MLS providers for one leaf.
- **Sign-ins.** A password, passkey, recover, TOTP or device-restore round trip
  that started before a sign-out or before another account landed could land
  afterwards and replace the session the person was in.
- **Late writes after a wipe.** A store write already past its "which account?"
  read when Forget device ran finished afterwards and put the forgotten
  account's rows back: on Android and iOS as a thread race (writers off the
  main thread or main actor), on web as an `await` resumed after the wipe's
  deletes, and on web also from in-memory caches saved back once the same
  account signed in again on the same page.
- **Publishing on a write that did not happen.** The web `onPersist` hooks went
  through `ChatContext.persistSnapshot`, which reported success with no
  session, identity or device key; the caller then published. An iOS chat send
  whose account left between the seal and the persist published a ciphertext
  with no persisted ratchet step. Membership proposals on iOS and Android
  persisted best-effort and published anyway. iOS DeviceGroup sync published
  before it persisted. On all three clients, founding a group published the
  Welcome before its best-effort persist.

## Decision

### 1. Account binding

Each owner of an account binding keeps an epoch and advances it whenever the
bound account changes: every landing and every sign-out (and Forget device). A
counter and not the account id alone, because signing out and back in to the
same account is a new binding with new handles. Work captures the epoch before
its first wait and asks after each one, before any write or act. A stale
binding ends the work as a **cancellation**, not an error: nothing is reported,
logged as a failure or retried.

- **Web:** `apps/web/src/auth/binding-epoch.ts`. `BindingEpoch`
  (`advance`, `capture`), `StaleBindingError`, `isAbandoned`, and
  `SessionBinding`, which counts sign-outs and landings apart (`signedOut()`
  advances both, `landed()` only the second) so that a flow can be stopped by
  a sign-out alone (`captureSignOut`) or by either (`capture`).
  `AuthContext.setState` calls `landed()` for every non-anonymous state.
  Providers (DeviceGroup sync, the mailbox poller, push subscribe, channel
  post sync, background receive) capture and check after every await.
- **iOS:** `apps/ios/Lakko/Shared/BindingEpoch.swift`, `@MainActor`, with
  `Captured.isCurrent` / `require()` and `StaleBindingError`, which the
  client's `Cancellation` helper reads as a cancellation.
- **Android:** `apps/android/app/src/main/kotlin/me/lakko/app/BindingEpoch.kt`.
  `capture()` returns a `Binding` with `isCurrent` / `ensureCurrent()` and
  `takeOver()` (decision 2); `StaleBindingException` is a
  `CancellationException`; `launchBound` launches fire-and-forget work as a
  child of the binding so `advance()` cancels it; `endPassUnlessCancelled`
  lets a loop that outlives one pass go on for the next account.

A web group-call start carries an attempt object instead of a latch
(`groupCallStartCheck` / `runGroupCallStart` in `group-call-host.ts`): every
step is followed by a check that the attempt is current, and a start that is
not builds nothing further, reports "left", and takes back an announcement it
already published, at most once.

### 2. An overtaken sign-in is dropped (maintainer, 2026-10-13)

A sign-in round trip (password, passkey, recover, TOTP, the device restore,
and on web the recovery-email password route) that started under a binding
which has since changed, by a sign-out or by another account landing, does not
land: nothing is installed, persisted or shown, and no error copy appears.
Contact recovery, which the 2026-10-13 commit names, no longer exists at
`3af913ad` (ADR-0072 removed the factor).

Sign-up is the exception: once the registration has finished on the server the
account is landed. As built:

- web `signup` asks no binding after its commit point; web guest signup lands
  whatever else landed, and a sign-out during the landing itself still stops
  it (`registered: true` selects `captureSignOut`);
- iOS `signup` and `signupGuest` ask no binding after the registration;
- Android `signup` and guest signup ask none after their commit point.

Android makes the check and the landing one step for a sign-in started signed
out: `Binding.takeOver()` passes only while the binding is current and ends it
in the same step, so of two sign-ins racing each other exactly one lands. A
sign-in started for the account already on the device asks
`ensureCurrent()` only.

### 3. A fresh identity's phrase waits for its account (maintainer, 2026-10-13)

`mintFreshIdentity` (ADR-0017's lost-everything fallback; at `3af913ad` the
ADR-0073 key reset, whose server step is the irrevocable one) must neither
show a new phrase over another account's session nor lose it. The rule:

1. The phrase is written to the account's own protected phrase store before
   the irrevocable step; a failed write refuses the mint.
2. After the step, the phrase is marked owed in every case, and shown now
   unless another account holds the device (web `freshPhraseDisposition`,
   iOS `FreshIdentityLanding`, Android `freshIdentityAfterPublish`).
3. The owed marker is cleared only by confirming the phrase or by Forget
   device, never by logout.
4. The phrase row is kept on confirm: the device seals its backup with it
   (ADR-0072 decision 8).
5. A later sign-in of that account that would park on the restore prompt
   completes from the kept phrase through the existing
   `provideRecoveryPhrase` check, which still refuses a phrase the published
   identity does not match (web `owedPhraseCompletesPark` /
   `completeParkFromOwedPhrase`, iOS `completeParkFromOwedPhrase`).
6. A phrase the person typed clears the owed marker: they have shown they hold
   it.

**As built at `3af913ad`, the three clients differ** (recorded as G-47):

- Point 1 holds on **Android only**: `StagedPhrase` stages the row before
  `resetAccountKeys`, refuses the reset with
  `restore_identity.lost.error.phrase_not_stored` when it cannot, and puts the
  previous row back if the server step fails. Web wrote the phrase before the
  publish at `c22ad3e3`; the account-recovery merge (`1ae14935`) replaced that
  flow with `adoptKeyResetOutcome`, which writes the phrase after the server
  step and ignores the `false` that `storeDevicePhrase` returns on a failed
  write. iOS `landKeyReset` also writes it
  after the server step (`DevicePhrase.store` does not throw).
- Point 2: web marks owed unconditionally (`owePhraseScreen`); iOS marks owed
  only when ADR-0075 shows new words (`NewWordsRule.shown`); Android marks
  owed and clears the marker at the next landing for an account that is not
  phrase-protected.
- Point 3 also has a third clear path on iOS: an owed phrase that is no longer
  readable clears the marker (`parkOwedFreshPhrase`).
- Point 5: Android keeps the seed with the phrase, so its next sign-in does not
  park and shows the owed phrase at the landing (`restoreOwedPhrase`).
- Point 6 holds on **Android only** (`phraseHeldByPerson` on `recover` and the
  restore prompt); web and iOS do not clear the marker when the phrase was
  typed.

### 4. A wiped account's stores take no late write

Each client gates its per-account store writes. Forget device (and Delete
account and the other full wipes) seals the account before its first delete
and waits for writes in flight; a sealed account's writes are refused until
that account lands again, and every sign-in route opens the account at its
commit point. The three durable MLS writes (the MLS snapshot, the commit
outbox, the DeviceGroup snapshot) **throw** when refused, because their
callers publish only after them and a persist that silently did nothing
followed by a publish reuses a ratchet position at the next sign-in; every
other store skips the write.

- **iOS:** `apps/ios/Lakko/Shared/AccountStoreGate.swift`, a process-wide
  `pthread_rwlock`. A store's mutator holds it shared for one write (disk,
  Keychain or `UserDefaults`; no network, no suspension) via `enter` /
  `enterOrEnd`; `seal(_:wiping:staged:during:)` takes it exclusively, so it
  waits for writes in flight, and `AccountLocalDataWipe` runs inside it.
  Logout seals and deletes nothing. `enterUnlessWiped` and
  `enterUnlessFullyWiped` serve the rows written for a signed-out account (a
  fresh identity's owed records, the ADR-0074 recall record); `WipeMark` refuses
  a sign-in's own pre-landing write only if the account was wiped after the
  work began. Compiled into the notification extension, which only reads.
- **Android:** `apps/android/app/src/main/kotlin/me/lakko/app/account/AccountStoreGate.kt`,
  a process-wide `ReentrantReadWriteLock`, the innermost lock a writer takes
  (inside the per-group mutex and the provider lock, outside a store's own
  monitor). `write` / `writeOrEnd` are shared; `seal(account) { wipe }` is
  exclusive; forget-device runs its whole local wipe inside it; logout and
  `ChatRepository.clear()` seal and delete nothing. `markWiped` /
  `writeUnlessWiped` give wipe-only semantics to the few rows written for a
  signed-out account (the email-recovery own-action marker among them). The
  anonymous token pool keeps a per-account forgotten mark under its own lock.
- **Web:** `apps/web/src/auth/account-store-gate.ts`. IndexedDB runs
  overlapping read-write transactions in creation order across connections and
  tabs, and every per-account store deletes rows, so the gate is asked
  synchronously on the line before each `readwrite` transaction: one created
  before the wipe's delete is ordered ahead of it, one created after is
  refused. `localStorage` / `sessionStorage` are covered by a
  `Storage.prototype.setItem` wrapper that drops a write whose key names a
  sealed account. Another tab learns of a wipe from a short-lived marker that
  holds a 64-bit FNV-1a digest of the account id (not the id) for 60 s. A
  plain sign-out seals the session-token row only (`sealAccountSession`): the
  module's argument is that a late write of the account's own rows after a
  sign-out belongs on disk. `account-memory.ts` registers the page-lifetime
  caches that hold an account's rows; the wipe drops them at its seal, and a
  landing of an account sealed by a wipe drops them again before its first
  write.

A long-lived handle of one session is refused after the same account signs in
again (iOS `AccountStoreGate.Session`, Android `AccountStoreGate.Session`):
before, an earlier session's DeviceGroup handle could write its whole provider
over the new session's DeviceGroup snapshot.

### 5. A refused persist means nothing was published

Every publish whose correctness depends on a persisted state (a ratchet step, a
commit, a proposal, a DeviceGroup step, a founder's state) persists first and
fail-closed, and a refused or failed persist stops the publish.

- Web: all 22 `onPersist` hooks use `durableSnapshotHook`
  (`apps/web/src/chat/persist-hooks.ts`), which resolves only when the
  snapshot was written and throws `NoSessionPersistError` without a signed-in
  account; the five proposal publishes persist fail-closed first.
- iOS: the send carries its binding and the persister refuses; proposals,
  DeviceGroup commits and DeviceGroup sync persist fail-closed before
  publishing (sync used to publish first); the channel publish counter is
  handed out only once saved.
- Android: proposals persist fail-closed before the wire.
- All three: founding a group writes the founder's MLS snapshot fail-closed and
  its known-groups row before any Welcome, and removes the row again if no
  Welcome goes out.

This extends ADR-0040's rule for the four membership-commit paths to every
publish, and adds the sealed account as one more reason a persist can refuse.

## Alternatives

- **A binding check alone, no store gate.** Rejected on iOS and Android: the
  late writes there are a thread race, not work resumed after a wait, so a
  check and the wipe can both pass at the same moment. The two sides have to
  exclude each other (the gate's module docs).
- **The account id instead of an epoch.** Rejected: a sign-out and a sign-in
  to the same account is a new binding with new handles, and work from the
  first must not complete into the second.
- **Rebuild the repositories per session.** Not taken: the owners are
  process- or page-lived by design (Android made the four repositories
  process-scoped in `AppRepositories` to stop two MLS providers for one leaf),
  and rebuilding would not cancel what the old instances had started.
- **Skip every refused write, including the durable ones.** Rejected: a skipped
  ratchet persist followed by a publish reuses a ratchet position at the next
  sign-in.
- **A lock in front of IndexedDB on web.** Not needed: the transaction order
  across connections and tabs already orders a write created before the
  wipe's delete ahead of it; only a write created after has to be refused,
  which a synchronous check does. This rests on the IndexedDB specification,
  not on a test (G-48).

## Consequences

Behaviour changes, each named in the commit messages:

- iOS logout clears the chat repository and its cached token. VoIP push is
  registered at sign-in (it was registered once per launch) under an explicit
  pseudonym, serially.
- Android: the account id is no longer zeroed at sign-out; session seeds are
  wiped through `SessionSecretWipe`, which refuses an all-zero seed; device
  sync is torn down before chat on logout and forget-device; the four
  repositories are process-scoped.
- iOS: a disk or Keychain failure now stops a DeviceGroup publish.
- Web: a session with an identity and no stored device key can no longer send
  or propose.
- Android: a person who signed in by typing the phrase is not shown it back.
- Cancellation is not a failure on every client: Android `rethrowIfCancelled`
  at the reporting catch sites, iOS a `Cancellation` helper; a screen shows no
  error for work the person left.
- An overtaken sign-in shows nothing; signing in twice in a row lands the
  second only.

Security invariants: **#4** is strengthened (no publish on the strength of a
write that was not made). **#1**, **#2**, **#5** and **#7** are untouched. **#3**
is untouched: none of the eight commits touches `crates/`, `schemas/` or
`services/`, and `git diff 180602c1^..3af913ad -- crates/lakko-mls` is empty.
(`git diff 180602c1^..3af913ad -- crates/` is not empty, but every crate change
in that range comes from the account-recovery branch merged in `1ae14935`, not
from this work.)

Verification, as recorded in the last commit message (not re-run for this
ADR): iOS `xcodebuild test` on the simulator, 3987 tests, 0 failures, 2
skipped; Android `testDebugUnitTest`, 4285 tests, 0 failures, 1 skipped; web
`tsc` clean and vitest 447 files, 6009 tests. Most binding guards are covered
by build and reading only; web `AuthContext` behaviour by pure modules and
source pins only.

## Known and left (state at `3af913ad`)

Tracked as G-47 (the phrase rule), G-48 (store-gate coverage) and G-49 (an
overtaken sign-in can still have changed server state, and binding residuals):

- The fresh-identity phrase rule (decision 3) differs across the clients as
  listed there.
- Closed by the commit after `3af913ad`, host-tested only: the iOS sign-in key
  writes (a wipe mark taken at the start of each sign-in route), the iOS stores
  that took the bound account at write time, the Android phrase store, the join
  request's persist-before-knock on all three clients, and the web service
  worker's per-URL cache keys (see G-48).
- Android and web sign-in key writes other than the phrase are not under a wipe
  mark; the iOS channel co-publisher request knocks before persisting.
- Android: the seal ends at the account's next landing, so a stale coroutine
  that resumes after the same account signed in again is stopped by its
  binding check only.
- Web seals only the session-token row at logout, iOS and Android seal every
  store of the account; the two models are argued differently in the code and
  were not reconciled.
- An abandoned recover has already changed the password on the server, and an
  abandoned one-time code is already consumed.
- iOS `JournalClaimOnOpen.ensure` writes its `settled` / `failedAtMs`
  bookkeeping after its await without a binding check; the claim it runs
  checks its own binding and answers `.failed` when overtaken.

Found on 2026-10-14 by the web residual pass after this work, outside decision
5's scope and recorded separately: the channel PublisherGroup commit paths
(co-publisher approve, rotation) publish first and persist afterwards through a
debounced, error-swallowing flush (G-50), and a question whether channel publish
counters restarting at 0 after Forget device can reuse a post nonce, which the
web code does not confirm (G-51).
