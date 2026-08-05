<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0041: iOS silent-push background decrypt in the notification-service extension

- **Status:** Proposed
- **Date:** 2026-08-05
- **Context:** iOS push notifications — background message-preview decrypt. The
  hard-platform slice of the silent-push arc already shipped on **web** (commit
  `148753b`) and **Android** (commit `ae9a087`); iOS is the one platform whose
  process model prevents reusing that shape as-is. This ADR designs the iOS arc.

## Context

The other two clients already render a **real** message preview on a push wake
instead of a generic "New message" banner, and they do it without corrupting the
foreground app's MLS ratchet. The shape on both is a **read-only throwaway
decrypt**:

- On a push wake, restore a **temporary** `Group` from the last-persisted MLS
  snapshot.
- Do a **read-only** DS fetch after the app's persisted cursor.
- Decrypt the fetched frames through the audited path
  (`processIncomingVerifyingRoles` for commits, `parseBodyEnvelope` for the
  application body).
- Render the real preview, then **DISCARD** everything — never persist the
  advanced snapshot, never advance the cursor.

Because the decrypt runs on a throwaway in-memory group that is never written
back, the foreground app's ratchet is never advanced behind its back, so the
G-34-class publish/persist-ordering ratchet-desync hazard (ADR-0040 / the G-34
AEAD-reuse audit) is structurally avoided: a background decrypt that advances the
receive ratchet and then vanishes leaves the durable snapshot exactly where the
foreground app left it. `lakko-security` reviewed the **web** slice and returned
**APPROVE-WITH-NOTES**: the read-only-throwaway is airtight there because the web
decrypt uses an in-memory MLS provider that holds no persistent handle, runs
**zero** write transactions, and never calls `snapshot_storage`.

**iOS cannot reuse the core the same way — it is the hard platform.** The reasons
are process-model facts, all confirmed against the current tree:

### The `UNNotificationServiceExtension` process boundary

`apps/ios/LakkoNotificationService/NotificationService.swift` is a
`UNNotificationServiceExtension` (NSE) — a **separate process** from the main app,
with a hard, tight RAM ceiling (the file's own header notes ~30 MB; the practical
NSE budget is in the ~24–30 MB range) and ~30 s of wall-clock. iOS invokes it
before displaying any APNs alert push that carries `aps.mutable-content = 1`.
**Today it decrypts nothing** — its only mutation is to set `threadIdentifier`
(and a short `subtitle`) from the wake payload's group-pseudonym `g` field so iOS
groups notifications per conversation on the lock screen. Its own header comment
already names the three blockers this ADR resolves (App Group container, a slim
decrypt-only framework, and MLS-state persistence sharing).

### The five decrypt inputs are all app-private today

The App Group entitlement `group.me.lakko.dev.shared` is provisioned on **both**
the main app (`apps/ios/Lakko/Lakko.entitlements`) and the NSE
(`apps/ios/LakkoNotificationService/LakkoNotificationService.entitlements`) — but
it is not yet read, there is **no shared Keychain access group** (a repo-wide grep
for `kSecAttrAccessGroup` is empty), and every input the decrypt needs lives in
app-private storage the NSE process cannot reach:

| Decrypt input | Store today | Backing | NSE-reachable today |
|---|---|---|---|
| AccountIdentity seed (`Identity.fromSeed` root) | `AccountIdentitySeedStore.swift` | Keychain generic-password, `AfterFirstUnlockThisDeviceOnly`, per-account, **64 raw bytes** (not wrapped under the device key) | No |
| MLS group snapshot | `IdentitySnapshotStore.swift` | file `Application Support/lakko-mls-snapshots/<accountIdB64>.bin` + a 32-byte AES-256 wrap key in Keychain | No |
| Per-group cursor (+ cached messages) | `MessageLogStore.swift` | file + an AES-256 wrap key in Keychain (`me.lakko.message-log-wrap.v1`) | No |
| Known-groups set | `KnownGroupsStore.swift` | `UserDefaults.standard` | No |

The **device root key is not needed**: iOS stores the AccountIdentity seed
directly in the Keychain as raw bytes (protected by the hardware-derived class
key), not wrapped under the non-extractable device key, so the NSE never needs the
device key to reconstruct the identity — it needs the seed itself.

### The framework is far too large for the NSE budget

`target/ios/LakkoCore.xcframework` is **333 MB** fat (arm64 device slice
**111 MB**, arm64+x86_64 simulator slice **222 MB**), and its own header estimates
**~50 MB linked**. `crates/lakko-ffi-uniffi` has **no cargo feature seam**: it
monolithically pulls `lakko-transport`, `lakko-calls`, `lakko-sfu-proto`,
`lakko-call-jwt`, `lakko-channels`, `lakko-backup`, `lakko-transparency`,
`lakko-attest`, `lakko-contacts`, `lakko-discovery-client`, and more, alongside the
`lakko-mls` + `lakko-attachments` the decrypt path actually needs. Linking the full
framework into a ~24–30 MB-ceilinged process is very likely to bust the budget on
load, before a single frame is decrypted. The DS fetch does **not** need the
framework — it is a plain HTTPS GET the NSE can do with Swift `URLSession`.

### The APNs header prerequisite is already fixed

The push arc requires the wake to be a **mutable-alert** push
(`apns-push-type: alert` + `aps.mutable-content = 1`), **not** a silent
`content-available` background push. That server-side fix already landed (commit
`9a2dd82`; `services/push-gw/src/providers/apns.rs:251` now sets
`apns-push-type: alert`), so the NSE fires reliably and iOS always shows a visible
notification. This is **Phase 0**, done.

## The load-bearing design decision

There is exactly one decision the maintainer must weigh, and it must be stated
plainly rather than buried in a migration step:

> **On-device MLS decrypt in the NSE is impossible unless the AccountIdentity seed
> — the single highest-sensitivity on-device secret — becomes readable by the NSE
> process.** `Identity.fromSeed(seed)` reconstructs the account identity from the
> 64-byte seed; the NSE cannot decrypt anything without it, and there is no
> server-side substitute (the server is zero-trust and never holds key material,
> invariant #2). Sharing the seed with a second process is the price of the
> feature.

`AccountIdentitySeedStore.swift`'s own header calls the seed **"the
highest-sensitivity on-device secret: leaking the AccountIdentity seed equals full
account-identity compromise (it is the recovery factor itself)."** So this is not a
routine plumbing change; it widens the trust boundary around that secret from one
process to two.

**What the widening is, precisely, and its mitigations — stated honestly:**

- The seed **stays hardware-backed and this-device-only.** It remains a Keychain
  generic-password item protected by the Secure-Enclave-derived class key
  (`AfterFirstUnlockThisDeviceOnly`), with **no iCloud Keychain sync**. The
  Keychain payload is still non-extractable — user space cannot read it even with
  root; invariant #7's "non-extractable, hardware-backed" property is preserved.
- What changes is the **process-access scope**: the item moves from the app's
  private Keychain domain into a **team-scoped shared Keychain access group**
  (`kSecAttrAccessGroup`) that only the **first-party** app + NSE (co-signed under
  the same Apple Developer team) can read. A third-party app cannot join a team's
  access group, so the widening is app → {app, first-party NSE}, not app → any app.
- The exposure window is bounded by `AfterFirstUnlockThisDeviceOnly`: the seed is
  readable only after the first unlock since boot, in either first-party process.

The honest residual: a compromise of the NSE process (a code-execution bug in the
extension, or in the slim decrypt framework it links) now reaches the account
seed, where before it reached only the notification payload. The blast radius of
"the NSE is compromised" grows from "can rewrite a lock-screen banner" to "can read
the account identity." That is a real widening, and it is why **this arc must go
through `lakko-security` before Phase 1 lands** (§ Consequences).

**The maintainer must accept the seed-sharing framing** — "the seed stays
hardware-backed + this-device-only + no-iCloud-sync, but its read scope widens from
the app to the app + a team-scoped first-party NSE, and on-device NSE decrypt is
impossible without it" — before the App Group migration (Phase 1) is implemented.

## Decision

Implement iOS silent-push NSE decrypt in **four phases**, mirroring the
already-shipped-and-reviewed web/Android read-only-throwaway discipline, with the
seed-sharing migration and the framework-size question isolated into their own
phases so the risky changes are staged and independently reviewable.

### Phase 0 — APNs `push-type` fix (server-only). **Done.**

Commit `9a2dd82`: `apns-push-type: alert` for the mutable-content NSE push, so the
extension fires and a visible notification is always shown. No client change. This
is the prerequisite that makes the NSE reachable at all; recorded here for
completeness.

### Phase 1 — App Group storage migration (the seed-sharing decision)

Make the five decrypt inputs readable by the NSE process, fail-closed and behind a
one-time migration:

1. **Shared Keychain access group.** Add a Keychain access group shared between the
   app and the NSE (declared in `apps/ios/project.yml`'s `keychain-access-groups`
   for both targets, matching the team prefix), and set `kSecAttrAccessGroup` on:
   - the **AccountIdentity seed** (`AccountIdentitySeedStore`) — the load-bearing
     item (§ above),
   - the **snapshot AES wrap key** (`IdentitySnapshotStore`),
   - the **cursor AES wrap key** (`MessageLogStore`).
   Keychain accessibility stays `AfterFirstUnlockThisDeviceOnly`; **no iCloud
   sync**; the access group is team-scoped (first-party only).
2. **Move the file-backed blobs into the App Group container.** Relocate the MLS
   snapshot file (`Application Support/lakko-mls-snapshots/...`) and the message-log
   file into the shared App Group container
   (`FileManager.containerURL(forSecurityApplicationGroupIdentifier:
   "group.me.lakko.dev.shared")`), so the NSE can read them.
3. **Move known-groups to a shared suite.** `KnownGroupsStore` moves from
   `UserDefaults.standard` to `UserDefaults(suiteName: "group.me.lakko.dev.shared")`.
4. **One-time migration.** Keychain items do **not** relocate to a new access group
   automatically, and files do not move themselves. On first launch after the
   update:
   - re-add each affected Keychain item into the shared access group (read from the
     old domain, add to the new, then delete the old — bounded, per-account),
   - copy the snapshot + message-log files into the App Group container with a
     read-from-old-location fallback until the copy is confirmed (so an interrupted
     migration never strands the foreground app), then remove the originals,
   - copy the known-groups set into the shared suite.
   The migration is **idempotent** (re-running finds the items already in the shared
   domain) and **fail-closed for the NSE** — the NSE never writes any of these
   stores, only reads, so a half-migrated state degrades the NSE to the generic
   fallback (§ Phase 3) without touching the foreground app's authoritative copy.

**This phase is the seed-sharing decision made concrete. It touches invariant #7's
trust boundary and must go through `lakko-security` before it lands.**

### Phase 2 — slim decrypt-only framework (conditional on a size measurement)

The decrypt path needs only `lakko-mls` + `lakko-attachments` (+ their transitive
`lakko-crypto` / `lakko-proto` / openmls). Add a slim
`crates/lakko-ffi-extension` crate exporting a **decrypt-only** UniFFI subset
(restore-throwaway-group / process-incoming-verifying-roles / parse-body-envelope,
plus the seed → `Identity.fromSeed` reconstruction), cross-compiled to a separate
`LakkoCoreExtension.xcframework` the NSE links **instead of** the 333 MB
`LakkoCore.xcframework`.

This phase is **conditional on a measurement**, not assumed: once Phase 1 lets the
NSE link a framework and run the restore, measure the actual linked/resident size
of the full `LakkoCore.xcframework` in the NSE. If it fits the budget (unlikely
given the ~50 MB linked estimate against a ~24–30 MB ceiling), Phase 2 is skipped
and the NSE reuses the existing framework. If it busts the budget (the expected
outcome), the slim framework is built. Deciding by measurement rather than
assumption avoids either a wasted crate-split or a shipped OOM.

### Phase 3 — the NSE read-only throwaway decrypt handler

Extend `NotificationService.didReceive` to decrypt, mirroring web/Android **exactly**:

- Read the shared inputs (seed from the shared-access-group Keychain; snapshot +
  cursor from the App Group container; known-groups from the shared suite),
  resolving the target group from the wake payload's `g` field.
- Reconstruct a **fresh throwaway `Identity`** from the seed and restore a
  **temporary** `Group` from the snapshot — **never** a live handle the foreground
  app holds (the app and NSE are separate processes; there is no shared in-memory
  handle to corrupt, but the NSE must still never write back).
- **Read-only DS fetch** after the persisted cursor via `URLSession` (no
  framework needed for the fetch).
- Decrypt through the audited path (`processIncomingVerifyingRoles` /
  `parseBodyEnvelope`), render the real preview into `bestAttemptContent`.
- **Never persist the advanced snapshot, never advance the cursor, never write any
  shared store.** Zero the seed bytes after use.
- **Fail-closed** on any error (missing input, budget exceeded, decrypt failure, a
  frame that will not authenticate): fall back to the existing generic
  "New message" content via `contentHandler(bestAttemptContent)` — the same
  fallback posture the NSE already implements for `serviceExtensionTimeWillExpire`.
  A frame that will not authenticate is **dropped**, never rendered (invariant #4).

**Foreground-state note.** The NSE cannot cheaply know whether the foreground app
is already displaying the conversation, so v1 **always** attempts the decrypt (it
is harmless — read-only, discarded). An optional refinement is an App-Group flag
the foreground app sets while a chat is open, letting the NSE skip the decrypt for
the active conversation; that is a battery/latency optimisation, not a correctness
requirement (§ Open questions).

The read-only-throwaway discipline here is **the same one `lakko-security` already
approved on web** — the only iOS-specific delta is the storage-sharing plumbing
(Phase 1) and the framework-size handling (Phase 2), which is why those carry the
review weight, not the decrypt logic itself.

## Alternatives

- **A true silent `content-available` background push (no NSE / no mutable-alert).**
  Rejected. iOS throttles and coalesces `content-available` background app
  execution aggressively and gives no guarantee the app runs promptly (or at all)
  on a given push, and a `background` push must omit the alert — so there is **no
  guaranteed visible notification** and no reliable decrypt window. The mutable-alert
  NSE model is what Apple actually guarantees fires per push and always shows a
  banner; keep it. (This is exactly why Phase 0 corrected the header **to** `alert`,
  not `background`.)

- **Load the full monolithic `LakkoCore.xcframework` in the NSE.** Rejected pending
  the Phase-2 measurement. The full framework's ~50 MB linked estimate against the
  ~24–30 MB NSE ceiling makes an OOM-on-load the expected outcome, and it drags
  transport / calls / SFU / channels code the decrypt path never touches into a
  memory-starved process. If the measurement surprises us and it fits, Phase 2 is
  skipped — but the design must not assume it fits.

- **Do not share the seed with the NSE (the null alternative).** Rejected — it is
  the "do nothing" option restated. Without the AccountIdentity seed the NSE cannot
  run `Identity.fromSeed`, so **no on-device decrypt is possible on iOS at all**,
  and the platform stays at today's generic "New message" banner while web and
  Android show real previews. The whole point of the arc is on-device decrypt; the
  seed-sharing is its irreducible cost. Recorded explicitly so the trade-off is on
  the table, not implied.

- **A separate second file/object for the shared MLS state instead of moving the
  authoritative store into the App Group.** Rejected. A mirror-write from the app to
  a second NSE-readable copy opens a staleness/torn-state window between the two
  copies (the mirror lags the authoritative store), and the NSE could decrypt
  against a stale snapshot. Moving the single authoritative file into the App Group
  container (read by both, written only by the app) keeps one source of truth with
  no mirror to skew — the same one-authoritative-copy discipline ADR-0037 chose for
  the backup blob.

## Consequences

- **Positive.** iOS reaches parity with web + Android: a real message preview on a
  push wake, using the **same** read-only-throwaway decrypt `lakko-security` already
  approved on web, so the ratchet-desync hazard is avoided by construction (the
  foreground snapshot + cursor are never advanced by the NSE). The phased structure
  isolates the two risky pieces — the seed-sharing migration and the framework-size
  handling — into their own reviewable steps, and Phase 2 is decided by measurement
  rather than assumption. The slim decrypt framework (if built) is reusable for any
  future extension that needs decrypt without the full client surface.

- **Negative / new risks.** The **AccountIdentity seed's read scope widens from one
  process to two** (app + first-party NSE) — the load-bearing risk (§ above): an
  NSE-process compromise now reaches the account seed, not just the notification
  payload. A shared Keychain access group + App Group container is new attack
  surface (team-scoped, first-party-only, but still a widened boundary). The
  one-time migration is a real correctness subtlety (Keychain items and files do not
  relocate automatically; a botched migration could strand the foreground app if the
  read-from-old fallback is wrong). If the slim framework is needed, maintaining a
  second FFI surface + a second xcframework is ongoing build/CI cost. The NSE's tight
  RAM + time budget means the decrypt can fail under load and degrade to the generic
  banner (acceptable — fail-closed).

- **Neutral.** iOS-only; no wire-format, schema, proto, or server change (Phase 0's
  server fix already landed separately). The decrypt logic is the audited path
  reused verbatim; the foreground app's storage layout changes location (App Group
  container / shared access group) but not its content or its authoritative-writer
  status. Steady-state foreground behaviour is unchanged.

### Invariants touched

- **#2 (server sees only ciphertext + public artefacts) — holds.** The NSE only
  does a read-only DS fetch (the same opaque frames the app already fetches) and
  decrypts locally; the server learns nothing new (it already routed the wake to the
  group-pseudonym `g`). No key material leaves the device.
- **#3 (the MLS state machine is sacrosanct) — untouched.** No `lakko-mls` change —
  the NSE runs the audited `processIncomingVerifyingRoles` / `parseBodyEnvelope`
  path on a **throwaway** restored group, exactly as the app does; the slim
  framework (Phase 2) re-exports the same crate, it does not fork it.
- **#4 (no silent downgrade / fail-closed) — holds.** A frame that will not
  authenticate is dropped and the NSE degrades to the generic "New message" banner;
  there is no non-E2EE fallback, only a less-informative-but-still-safe banner.
- **#7 (hardware-backed device keys) — the one under tension.** The AccountIdentity
  seed stays hardware-backed (Secure-Enclave-derived class key), this-device-only
  (`AfterFirstUnlockThisDeviceOnly`), and non-iCloud-synced — the letter of #7
  (non-extractable, hardware-backed) is preserved. What widens is the **process
  scope** that may read it: from the app alone to the app + a team-scoped first-party
  NSE. This is the reason the arc must be reviewed before Phase 1 lands.
- **Invariant-index touchpoints (do not edit now — Proposed, not landed).** This
  arc touches the iOS on-device secret-storage posture. If accepted and landed it may
  warrant a new **C-entry** (or a free-form gap note, mirroring how G-33/G-34/G-35
  were kept free-form) recording the NSE seed-sharing boundary + the read-only-
  throwaway decrypt discipline. The current index (**8 I + 58 C + 15 T**, highest
  gap **G-35**) is **not** edited by this Proposed ADR — that reconcile happens on
  acceptance/landing, per the docs-steward convention.

**Flag this ADR + the Phase 1 (seed-sharing / App Group migration) and Phase 3
(NSE decrypt) implementation through `lakko-security` before Phase 1 lands**
(maintainer-invoked, per the crypto/auth gate). The review should confirm: the
shared Keychain access group is genuinely team-scoped (first-party-only) with
`AfterFirstUnlockThisDeviceOnly` + no iCloud sync intact; the widened seed-read
boundary is the accepted minimum for on-device decrypt; the migration cannot strand
or corrupt the foreground app's authoritative stores; and the NSE decrypt path
performs **zero** writes to any shared store and never advances the app's
snapshot/cursor — the same read-only-throwaway property approved on web.

## Open questions

- **Phase-2 go/no-go.** Whether the slim `lakko-ffi-extension` framework is needed
  is a measurement Phase 1 unblocks: link the full `LakkoCore.xcframework` into the
  NSE, run the restore, measure resident size against the budget. Expected outcome is
  "slim framework required"; the measurement decides, not an assumption.
- **Foreground-suppression refinement.** Whether to add an App-Group flag the
  foreground app sets while a chat is open so the NSE skips the (harmless) decrypt
  for the active conversation — a battery/latency optimisation, deferred.
- **Dedicated NSE seed-access posture.** Whether to further constrain the NSE's seed
  read (e.g. a distinct access-group item with tighter `kSecAttrAccessible`, or a
  short-lived derived sub-key instead of the raw seed) to shrink the blast radius of
  an NSE compromise. Deferred as a defence-in-depth follow-up for `lakko-security` to
  weigh — v1 shares the raw seed under the team-scoped access group.
- **Migration rollback safety.** The exact ordering of the one-time Keychain +
  file + suite migration (copy-then-verify-then-delete, with the read-from-old
  fallback) is the one genuinely fiddly implementation point; Phase 1 must nail it
  per store so an interrupted update never strands the foreground app.
- **macOS Catalyst / other extensions.** Whether the same shared-container plumbing
  is reused by a future macOS notification path or another extension is out of scope
  here (this ADR is the iOS NSE decrypt) — noted for parity if it comes up.

## Sprint decomposition

- **Phase 0 — APNs `push-type` fix.** **Done** (commit `9a2dd82`,
  `services/push-gw/src/providers/apns.rs`). Server-only; no client change.
- **Phase 1 — App Group storage migration.** The shared Keychain access group +
  `kSecAttrAccessGroup` on the seed / snapshot-wrap-key / cursor-wrap-key; the
  snapshot + message-log files into the App Group container; known-groups into
  `UserDefaults(suiteName:)`; the idempotent, fail-closed, one-time migration. **The
  seed-sharing decision — `lakko-security` gate before this lands.**
- **Phase 2 — slim decrypt-only framework (conditional).** Measure the full
  framework in the NSE first. If it busts the budget (expected), build
  `crates/lakko-ffi-extension` ({`lakko-mls`, `lakko-attachments`}) →
  `LakkoCoreExtension.xcframework`, cross-compiled and linked by the NSE instead of
  `LakkoCore.xcframework`.
- **Phase 3 — NSE read-only throwaway decrypt handler.** Extend
  `NotificationService.didReceive` to reconstruct a fresh throwaway `Identity` from
  the shared seed, restore a temporary group, do a read-only `URLSession` DS fetch
  after the cursor, decrypt via `processIncomingVerifyingRoles` /
  `parseBodyEnvelope`, render the real preview, then discard — never persist, never
  advance the cursor, zero the seed, fail-closed to the generic banner. Mirrors
  web/Android exactly.

## References

- **Web** commit `148753b` (`feat(web): silent-push background decrypt`) — the
  reference read-only-throwaway implementation `lakko-security` reviewed
  (APPROVE-WITH-NOTES: in-memory provider, zero write transactions, no
  `snapshot_storage` call).
- **Android** commit `ae9a087` (`feat(android): silent-push background decrypt`) —
  the second landed client mirroring the same discipline.
- **Phase 0** commit `9a2dd82` (`fix(push-gw): correct APNs push-type to alert for
  the mutable-content NSE push`) — the server prerequisite.
- `apps/ios/LakkoNotificationService/NotificationService.swift` — the NSE today
  (threadIdentifier-only), whose header already enumerates the three blockers this
  ADR resolves.
- `apps/ios/Lakko/Auth/AccountIdentitySeedStore.swift` — the AccountIdentity-seed
  Keychain store (the load-bearing secret; the header naming it "the
  highest-sensitivity on-device secret").
- `apps/ios/Lakko/Chat/IdentitySnapshotStore.swift`,
  `apps/ios/Lakko/Chat/MessageLogStore.swift`,
  `apps/ios/Lakko/Chat/KnownGroupsStore.swift` — the snapshot / cursor / known-groups
  stores the migration relocates.
- **ADR-0017** — phrase-seeded AccountIdentity (`Identity.fromSeed`); the seed the
  NSE must read to reconstruct the identity.
- **ADR-0040** / the **G-34** AEAD-reuse audit — the publish/persist ratchet-desync
  hazard class the read-only-throwaway decrypt structurally avoids (the NSE never
  advances the durable ratchet).
- **ADR-0037** — the one-authoritative-copy discipline (backup blob) the App Group
  container migration mirrors (no second mirror-copy to skew).
