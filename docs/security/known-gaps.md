<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — Known gaps before public beta

This file is the **honest inventory**. Reviewers should
not re-discover these as findings; they are known and
tracked. Each entry has a target sprint / ROADMAP link.

Items here are either:

- (a) explicitly deferred to a later phase (the protocol
  shape is settled but the implementation lands later); or
- (b) known soft spots in the current implementation that
  we trade off for shipping speed, with a documented
  follow-up.

## MLS state machine

### G-1: Channel two-tier flow (>50k subscribers) — **crypto + service + UI + nav + RotationCron + batched fanout + multi-epoch retention + `@channel` client discovery BUILT (Sprints A–D, 2026-07); residual is the deploy-gated >50k scale proof only**

Phase 4 (ADR-0010, two-tier PublisherGroup + sender-key broadcast). A 2026-07
code audit found the prior "nothing is shipped" note **badly stale** — the crypto
substrate, backend, both FFI surfaces, discovery, and publisher / subscriber /
co-publisher UI on all three clients are built. What is built vs the real residual:

**Built:**

- `crates/lakko-channels` (4a/4b): `seed` (MLS-exporter content-key seed), `post`
  (per-message AEAD off the seed + Ed25519 signature — `compose_post` /
  `verify_and_decrypt`), `subscribe` (HPKE-equivalent seed envelope
  `seal_seed`/`open_seed`, 80-byte wire, AAD-bound to `channel_id‖epoch`),
  `rotation_policy` (`should_rotate`), 4 fuzz targets.
- `services/channels` (4c): register / posts (publish + cursor fetch) /
  subscribers / envelope-deliver+pull / atomic `POST /rotate` fanout / epoch
  routes; in-memory + MariaDB `ChannelStore`; retention prune; opacity-preserving
  (crate is dev-dep only).
- FFI (uniffi + wasm): full channel + subscriber + `PublisherGroup` surface.
- Clients (web / iOS / Android): create+publish, co-publisher (ADR-0028 OQ3),
  subscribe-to-another's-channel, and a read-posts feed are all **real screens**
  with real MLS-backed crypto.
- `services/discovery`: `@channel` OPRF `register` / `lookup` (server side).

**Landed since the 2026-07 audit (Sprints A–D):**

1. **Navigation (Sprint A).** Channels is now reachable from app navigation — a
   nav entry on web (`AppShell`) + a Home call site presenting the channels screen
   on iOS (`HomeScreen`/`LakkoApp`) and Android (`HomeScreen`/`MainActivity`). No
   longer built-but-unreachable.
2. **RotationCron activation (Sprint A).** Wired to run on all three clients
   (heartbeat-driven scheduled rotation + a "Rotate now" affordance + persistence)
   — ADR-0010's aggressive-rotation backward-secrecy mitigation is live. The
   ROADMAP-PHASE3-CLOSEOUT G-27 "rotation wired via RotationCron on all three"
   wording is now accurate.
4. **Multi-epoch envelope retention (Sprint C, [ADR-0031](../adr/0031-multi-epoch-envelope-retention.md)).**
   N=1 → N=4: `channel_envelopes` keyed `(channel, subscriber, epoch)` + a
   prune-to-N on every write path; a client-side `epoch → seed` map + a
   `fetch_envelopes` catch-up + decrypt-by-epoch (a missing-epoch post renders an
   "older key window" placeholder and is **skipped**, never force-decrypted, #4) +
   a re-subscribe on offline-past-N. Posts made under a missed rotation are
   readable again within the retention window; only pre-window posts stay
   unreadable (an availability cost, not a confidentiality break).
5. **Batched rotation fanout (Sprint D, [ADR-0030](../adr/0030-batched-rotation-fanout.md)).**
   The single unbounded `POST /rotate` is joined by a chunked, resumable, idempotent
   staged prepare (`/rotate/stage`, ≤5000/chunk, `channel_staged_envelopes` never
   served) + an atomic `/rotate/commit` — C-42 preserved. Fit for the 50k–1M
   fanout the single request could not carry.
3. **`@channel` discovery (Sprint B).** DONE on web + iOS + Android — clients
   register a `@handle` at channel-create + resolve `@handle` → `channel_id` via
   the OPRF discovery (`registerChannelName` / `lookupChannel`, reusing the
   `@username` OPRF plumbing), so a user subscribes by `@handle` instead of a raw
   hex id (the hex-paste path is preserved). The server side
   (`/discovery/channel/{register,lookup}`) was already done; this is client
   wiring only, no FFI change.

**Remaining residual:**

6. >50k **operational scale** (dedicated HPKE-envelope bandwidth allowance, load
   test p95 < 5 s) — deploy-gated, not yet exercised.

**Recommended next sprint:** wire `@channel` discovery into the three clients
(item 3, Sprint B) — the last usability gap before the deploy-gated scale proof
(item 6).

### G-2: MLS-2 / quantum-hybrid suites pinned for later

The IETF MLS WG is still landing hybrid PQ cipher suite
identifiers. We pin classical suites today and migrate
once the draft stabilises.

### G-3: External-join proposal approval — **CLOSED end-to-end on web + iOS + Android ([ADR-0027](../adr/0027-external-join-approval.md) admin approval + [ADR-0028](../adr/0028-group-join-ticket.md) join-ticket invite-link); the only residual is a deployed-env live-admin-approval e2e**

Was: `Group::process_incoming` returned a data-less
`Incoming::ExternalJoinProposal` unit variant and the
clients ignored it — no joiner identity, no approve path.

**Phase 1 landed (Jun 2026, ADR-0027).** The `lakko-mls`
core now surfaces the joiner identity and gates an admin
approve:
- `Incoming::ExternalJoinProposal { joiner_signature_key }`
  — extracted from the external `Add` proposal's KeyPackage
  (refusing a non-`Add` / non-`NewMemberProposal`-sender
  proposal, defence in depth), retained in an in-memory
  per-group queue bounded by `MAX_PENDING_EXTERNAL_JOINS`
  (anti-spam) + de-duped by signature key.
- `Group::approve_external_join(identity, proposal_ref)` —
  **role-gated** (ADR-0022: owner/admin only on a group with
  a role table, fail-closed; open group stays open) — adds
  the joiner via the audited `add_member` path (Commit +
  Welcome); peers re-verify the resulting `Add`'s committer
  role on receipt, so a forged local approve can't take
  effect on them either.
- `Group::reject_external_join(proposal_ref)` (local discard,
  no epoch advance) + `pending_external_joins()` accessor.
- 5 property tests (owner-approves-adds-joiner /
  reject-removes-without-adding / dedup-by-signature-key /
  unknown-ref-errors / non-admin-approve-refused); full
  `lakko-mls` suite + clippy green.

**Phase 2 landed (FFI surface).** `ProcessedMessage`
carries `joiner_signature_key` (uniffi field /
`externalJoinSignatureKey` wasm getter), and the Group FFI
exposes `approveExternalJoin` (→ `AddMember` commit+welcome)
/ `rejectExternalJoin` / `pendingExternalJoins` on uniffi
(Swift/Kotlin) + wasm (web). `regen-bindings` + the web
`.d.ts` emit them.

**Phase 3 landed (Welcome rendezvous transport
primitive).** `crates/lakko-mls/src/welcome_rendezvous.rs`
`WelcomeRendezvous` — the carrier the admin posts to the
group journal (`version ‖ sig_key_len ‖ joiner_signature_key
‖ welcome`, bounded + fail-closed + panic-free, 8 unit tests
+ a `welcome_rendezvous_decode` fuzz target); FFI
`wrapWelcomeRendezvous` (admin) + `welcomeRendezvousFor`
(web non-throwing matcher) / `parse_welcome_rendezvous`
(uniffi). The Welcome is HPKE-sealed to the joiner's init
key, so the DS stores it opaque (invariant #2).

**Phase 4 client UI landed (members-UI approve/reject on
web + iOS + Android).** Each client's chat poll loop
classifies an `ExternalJoinProposal` into a dedicated
`externalJoinRequest` event carrying the joiner key (not a
timeline bubble — it drives a members-panel pending list via
`group.pendingExternalJoins()`, the MLS-core retained set
being the source of truth). An owner/admin-only
members-panel section (same role gate as add/remove, hidden
when empty) renders pending requests with Approve — which
drives the role-gated `approveExternalJoin`, broadcasts the
resulting commit to the group journal, wraps the Welcome in
a `WelcomeRendezvous` and posts it to the same journal, and
suppresses the sender's own commit echo — and Reject (a
local drop of the retained proposal, no commit/journal
post). Existing members skip the rendezvous entry (not an
MLS message → their receive loop's `processIncoming`
rejects + drops it). Verified: web `tsc`/`vitest` (213)/
`vite build`; iOS `xcodebuild` BUILD SUCCEEDED; Android
`:app:compileDebugKotlin` BUILD SUCCESSFUL (the 7 failing
Android unit tests are pre-existing Ktor-mock / base64
environmental failures, confirmed identical on a clean tree
— no chat test regressed).

**Joiner-side core + FFI landed.**
`Identity::create_external_join_proposal(group_id, epoch)`
(`crates/lakko-mls/src/key_package.rs`) lets a non-member
craft the external `Add` proposal (`JoinProposal::new`) for a
group's id+epoch, embedding a fresh KeyPackage whose private
keys persist in its own provider so the same identity later
opens the Welcome; FFI-exposed as `createExternalJoinProposal`
on UniFFI (Swift/Kotlin) + wasm. A full **in-process e2e
property test**
(`joiner_api_full_loop_proposes_approves_joins_via_rendezvous`)
proves the whole G-3 loop end-to-end: a non-member proposes
→ an admin's `process_incoming_verifying_roles` surfaces it →
`approve_external_join` → the Welcome rides a
`WelcomeRendezvous` → the joiner matches it by its own
signature key + `join_from_welcome` → it is a member at the
post-approve epoch (lakko-mls 95 lib tests green, clippy +
fmt clean).

**Join-ticket core + FFI landed
([ADR-0028](../adr/0028-group-join-ticket.md) Phase 1).** The
"convey the group params to a non-member" carrier was
reframed: ADR-0010 channel *subscribers* are not MLS members
(sender-key broadcast), so the join-ticket is a **general
group invite-link** for adding a member/publisher, not an
ADR-0010 artefact. A capability-based `/g/<slug>` link
(mirroring [ADR-0009](../adr/0009-link-based-call-invitations.md))
carries only the `group_id`; the joiner reads the **current
epoch from the journal's public MLS framing**
(`lakko_mls::epoch_of_message` — openmls
`ProtocolMessage::epoch()` works without a group secret), so
the ticket is epoch-independent (no stale-on-commit). The
security boundary stays the admin approval gate, **not** the
ticket — a leaked link lets a stranger *ask*, never *in*.
Landed: `crates/lakko-contacts/src/group_invite.rs`
(`GroupInviteCapability` + `compose`/`decode_and_verify`, 14
unit tests + the `group_invite_decode` fuzz target) +
`epoch_of_message` (1 test) + FFI
(`composeGroupInviteCapability` / `GroupInviteCapability` /
`epochOfMessage` on UniFFI + wasm). lakko-contacts 141 + 
lakko-mls 96 lib tests green, clippy + fmt clean.

**Phase 2 landed (owner create-link UI, web + iOS +
Android).** A members-panel owner/admin-only "Create invite
link" button (same role gate as add/remove) signs a
`GroupInviteCapability` over the group's id and shows the
copyable `https://lakko.me/g/<base64url>` link. Verified web
`tsc`/`vitest`/`vite build`, iOS `xcodebuild`, Android
`:app:compileDebugKotlin`.

**Phase 3 web landed (joiner flow — the live requester).**
`contacts/client.ts` `requestToJoinFromLink` (decode +
verify the capability fail-closed → read the current epoch
from the journal head's public framing via `epochOfMessage`,
no group secret → `createExternalJoinProposal` → post the
proposal to the journal) + `pollForJoinWelcome` (poll →
`welcomeRendezvousFor` → the audited `joinGroupFromWelcome`).
A public `/g/:slug` route + `JoinGroup.tsx` screen drives
decode → "Request to join" → poll-loop → navigate to the
joined chat (anonymous visitors prompted to log in, mirroring
`/i/`). This is the **live requester** that produces a real
pending request — G-3 is now exercisable end-to-end on web
(modulo a live admin approving). Verified `tsc`/`vitest`
(213)/`vite build`.

**Phase 3 native landed (iOS + Android joiner) — G-3
CLOSED end-to-end.** `ChatRepository.requestToJoinFromLink`
+ `pollForJoinWelcome` mirror the web orchestration (decode
+ verify the capability fail-closed → read the epoch from
the journal head's public framing → `createExternalJoinProposal`
→ post → poll → `parseWelcomeRendezvous` match → the audited
`joinGroupFromWelcome`); a `/g/` deep-link (the extended
`InviteIntake` + `MainActivity.handleGroupJoinIntent` /
iOS `onOpenURL`, plus the Android manifest
`pathPrefix="/g/"`) routes to a `JoinGroupLandingScreen` that
drives request → poll → join. Verified iOS `xcodebuild`
BUILD SUCCEEDED + Android `:app:compileDebugKotlin` BUILD
SUCCESSFUL.

**G-3 is now implemented end-to-end on web + iOS + Android**
— the admin approval (ADR-0027) + the join-ticket invite-link
(ADR-0028) form a complete joiner-initiated external-join
flow. ADR-0028's anti-abuse hardenings shipped: OQ1 (single-use
/ max-uses tickets via a server blob ledger + per-joiner
consume dedup — `services/discovery/src/group_invite.rs`,
migrations 0014/0015, wired on all 3 clients) and OQ2 (surface
"via &lt;issuer&gt; · member / not a current member" per pending
request from
`crates/lakko-contacts/src/group_join_attribution.rs` in the
members panel — advisory only, since the ADR-0027 role gate,
not roster membership, is the boundary). The only remaining
residual (deferred, not a gap in the protocol) is a live
admin-approval e2e on a deployed environment.

### G-35: MLS group-commit publish-before-persist → deterministic single-device epoch-desync brick — **CLOSED (2026-07, [ADR-0040](../adr/0040-durable-pending-commit-outbox.md) durable pending-commit outbox, S2–S5 landed on web + iOS + Android)**

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

### G-4: AccountIdentity rotation on password change has no KT-log supersession proof yet — **proof + chain verifier + FFI landed (sprints 1-3)**

The recover flow rotates the exportKey → rotates the
deterministic AccountIdentity. Today the old identity is
simply replaced; sibling peers will see a new pubkey on
the next handshake and TOFU it. The ADR-0007 §6 plan is
to publish a supersession proof to the KT log that
attests the old key → new key transition for ≤ 30 days,
so peers can verify the rotation without bouncing the
trust.

**Sprint 1 landed — the supersession proof primitive**
(`crates/lakko-transparency/src/supersession.rs`). A compact
fixed-layout Ed25519-signed blob
`version‖old_pubkey(32)‖new_pubkey(32)‖issued_at(8 BE)‖expires_at(8 BE)‖sig(64)`
(145 B) signed by the **old** identity key over the **new** key.
`SupersessionProof::sign` (rotating side) + the strict
`verify_supersession(bytes, trusted_old_pubkey, now)` (peer side:
signature against the embedded old key → the embedded key equals
the caller's trusted pin → a `<= 30-day` validity window
([`MAX_VALIDITY_SECS`], so a leaked old key cannot indefinitely
re-authorise, invariant #6) → freshness) returning the authorised
new key. `#![forbid(unsafe_code)]`, bounded parser, 7
unit/property tests (roundtrip, per-field tamper, stranger-forge,
old-key-mismatch, expiry, 30-day-window, malformed envelope).
**Sprint 2 landed — the peer-side chain verifier.**
`verify_supersession_chain(pinned, evidence, now)` ties the proof
to the KT log: it checks the operator's `SignedRoot` signature →
no-rollback vs the peer's last-seen root → the rotation
`LogEntry`'s Merkle inclusion under that root → the entry is for
this account (`username_hash` matches the pin) → a strictly newer
version → the supersession proof (old key signed, fresh, ≤30 d)
authorises **exactly** the entry's new key. So a rotation is both
cryptographically authorised AND non-equivocable, and the peer
re-pins to the returned `RotatedIdentity` without a blind TOFU.
This addresses the sprint-1 review's malleability note
structurally: the Merkle leaf commits to the
`(username_hash, version, pubkey)` tuple, **not** the supersession
signature bytes, so Ed25519 point-malleability cannot fork a log
entry. Six new tests (valid chain / wrong account / non-newer
version / proof-for-a-different-key / expired proof / forged
root). **Sprint 3 landed — the FFI surface for the proof primitive.**
`sign_supersession_proof(old_ed25519_seed, new_ed25519_seed, issued_at,
expires_at)` + `verify_supersession(proof, trusted_old_pubkey, now)` are
exposed on both UniFFI (`lakko-ffi-uniffi`, Swift / Kotlin) and
wasm-bindgen (`lakko-ffi-wasm`, web — `signSupersessionProof` /
`verifySupersession`, `i64` timestamps as `bigint`, byte slices as
`Uint8Array`, errors thrown). The sign helper takes each identity's
`ed25519_seed()` and derives both public keys internally, so the proof's
embedded `old_pubkey` always matches the signing key; verify returns the
authorised new key to re-pin. The chain verifier
(`verify_supersession_chain`) stays Rust-internal for now — it consumes
KT-log types (`SignedRoot` / `LogEntry` / `InclusionProof`) that have no
FFI wire format yet; that surface lands with the service. Four UniFFI
round-trip / reject tests; web `.d.ts` regenerated. **The transport +
client wiring landed (ADR-0016, Carrier B — the discovery identity
bundle).** Sprints 0-5 are wired end-to-end on web + iOS + Android
(build + unit-test verified): the discovery `identity_bundles` row carries
an opaque optional `supersession_proof` (migration 0010, server never
parses it — invariant #2); the recover flow emits a proof **atomically**
(capture the OLD exportKey before overwrite → sign old→new over a 14-day
window → upload `{new keys + proof}`, or upload nothing if it can't mint —
never a self-downgrade); and a per-contact pin store keyed on `account_id`
+ a fail-closed `verifyBundleAccountKey` gate at every `identityFetch` site
re-pins on a valid proof and **refuses** (typed `SupersessionRefused`, OOB)
a changed key without one — no blind TOFU. A malicious server can only
**strip** the proof (→ client fails closed), an availability impact never
an integrity break. **Remaining:** a `lakko-security` pass on the verify
path (explicit-request gate) + a live 2-device cross-platform e2e; and —
separately, for full equivocation resistance — the auth / transparency
*service* serving the KT-log entries that `verify_supersession_chain`
consumes (the chain verifier stays Rust-internal until then).

ROADMAP: Phase 2c (transparency log).

### G-5: Commit broadcast on `add_device_from_wire` to other pre-paired devices — **CLOSED**

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

### G-6: History bundle `payload_bytes` placeholder — **cross-platform interop CLOSED; raw-ApplicationPayload-bytes aspiration still open**

`MessageLogStore` carries the projected `(text, attachments,
direction, createdAt)` shape — not the original wire
`ApplicationPayload` bytes. The history bundle round-trips
that shape as opaque JSON inside `payload_bytes`. Both
sides agree on the placeholder; the wire schema stays
proto-clean (`payload_bytes` is just bytes to the schema).

A future sprint can teach the message log to retain the
original wire bytes (smaller + lets a future "verify-replay"
step re-process every old message through the chat envelope
parser) and swap the placeholder for the real thing.

**Audited 2026-06-04 — the placeholder is currently
same-platform-only (web↔native is non-functional).** Reading
the three codecs end-to-end surfaced that the placeholder is
NOT cross-platform-interoperable today, which is broader than
the "JSON field-ordering" framing G-9 carried:

* **`HistoryMessage.message_id` field encoding diverges.** Web
  ([`history-bundle.ts`](../../apps/web/src/devices/history-bundle.ts))
  sends `textEncoder.encode(m.messageId)` — the UTF-8 bytes of
  the **decimal string**. iOS
  ([`HistoryBundleCodec.swift`](../../apps/ios/Lakko/Devices/HistoryBundleCodec.swift))
  and Android
  ([`HistoryBundleCodec.kt`](../../apps/android/app/src/main/kotlin/me/lakko/app/devices/HistoryBundleCodec.kt))
  send the **8-byte big-endian u64**. Different bytes for the
  same logical id.
* **`payload_bytes` placeholder shape diverges.** Web encodes
  `PersistedMessage.messageId` as a JSON **string** (the type
  is `string`); iOS (`Int64`) and Android (`Long`) encode it
  as a JSON **number**. A native `JSONDecoder` /
  `kotlinx.serialization` decode of web's string-typed field
  throws → the message is silently dropped (`continue`); web's
  `JSON.parse` of a native number lands a non-string where the
  `PersistedMessage` contract says string, mis-driving the
  downstream `messageId` string ops.

Net: **iOS↔Android history catch-up interoperates** (matching
field encodings + order-tolerant decode); **web↔native does
not** — a cross-platform pairing falls back to "joiner starts
with no history" (no data loss, but the catch-up is a no-op).

The proper fix closes G-6 and the interop gap together: move
the placeholder encode/decode into the **shared Rust core**
(one canonical codec exposed via UniFFI + wasm-bindgen, so all
three platforms encode/decode identically and it is unit-
testable in Rust), OR ship the raw `mls_app::v1::ApplicationPayload`
proto bytes directly.

**Step 1 landed (2026-06-04) — shared-core codec.**
[`crates/lakko-mls/src/history_placeholder.rs`](../../crates/lakko-mls/src/history_placeholder.rs)
is the canonical codec: a versioned, bounded, deterministic
binary layout carrying `(message_id: u64, direction, created_at,
text, attachments[{blob_id, key, nonce, size, mime?, name?}])`.
`message_id` / `size` ride as typed `u64` (each platform renders
its own way — web decimal string, native integer); `created_at`
rides as the platform's display string verbatim. Exposed
identically through both FFI crates — `encode_history_placeholder`
/ `decode_history_placeholder` + records in
`lakko-ffi-uniffi` (Swift / Kotlin) and `encodeHistoryPlaceholder`
/ `decodeHistoryPlaceholder` + classes in `lakko-ffi-wasm` (web)
— so every platform produces and consumes the **same bytes**.
13 Rust unit tests cover round-trip, all directions,
determinism, and bounded-parse rejection (wrong version,
truncation, over-cap length, non-UTF-8, bad direction tag,
trailing bytes).

**Step 2 landed (2026-06-04) — per-platform wiring.** All three
clients now call the shared FFI codec for `payload_bytes`, and
the sibling `HistoryMessage.message_id` field converged on the
native 8-byte big-endian `u64` (web previously sent UTF-8 of a
decimal string):

* **web** ([`apps/web/src/devices/history-bundle.ts`](../../apps/web/src/devices/history-bundle.ts))
  — `encodeMessagePlaceholder` / `decodeMessagePlaceholder` call
  `encodeHistoryPlaceholder` / `decodeHistoryPlaceholder`
  (wasm), base64-decoding the attachment refs to raw bytes; the
  `message_id` field is now `messageIdToBeBytes` (BE-u64).
  `tsc --noEmit` clean.
* **iOS** ([`apps/ios/Lakko/Devices/HistoryBundleCodec.swift`](../../apps/ios/Lakko/Devices/HistoryBundleCodec.swift))
  — `encodePlaceholder` / `decodePlaceholder` over the UniFFI
  `HistoryPlaceholder` record; `message_id` already BE-u64.
  Verified: `LakkoCore.xcframework` rebuilt (`just build-ios`)
  with the new FFI symbols, then `xcodebuild build -scheme Lakko
  -destination 'generic/platform=iOS Simulator'` exited 0 with
  zero errors — the app compiles + links against the rebuilt
  framework.
* **Android** ([`apps/android/.../devices/HistoryBundleCodec.kt`](../../apps/android/app/src/main/kotlin/me/lakko/app/devices/HistoryBundleCodec.kt))
  — `encodePlaceholder` / `decodePlaceholder` over the UniFFI
  `HistoryPlaceholder` data class; `message_id` already BE-u64.
  `:app:compileDebugKotlin` BUILD SUCCESSFUL.

All three feed the same Rust `encode`/`decode`, so the
`payload_bytes` bytes agree and the `message_id` field agrees —
a web↔native history catch-up now interoperates by construction.
A live cross-platform pairing roundtrip is not headlessly
runnable here; the codec-layer determinism + 13 Rust unit tests
+ web tsc + Android `compileDebugKotlin` + iOS `xcodebuild` (with
the rebuilt xcframework) are the evidence. **The cross-platform
interop bug (the G-9 concern and the web↔native break) is
closed.**

**Still open (downgraded to a nice-to-have):** the placeholder
is a canonical *projection* `(message_id, direction, text,
created_at, attachments)`, not the raw
`mls_app::v1::ApplicationPayload` wire bytes. Carrying the raw
bytes instead would be marginally smaller and would unlock a
future "verify-replay" step (re-parse every catch-up message
through the chat-envelope parser). That is a separate,
non-blocking enhancement — the interop and byte-stability
problems are fully resolved without it.

### G-7: DeviceGroup snapshot persistence may briefly diverge across in-flight ratchet operations — **CLOSED**

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

### G-8: Web message-log change listener — **CLOSED**

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

### G-9: Web `payload_bytes` JSON placeholder strings are NOT byte-stable — **CLOSED**

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

### G-10: Backup envelope rotation folded into the recover round-trip — **CLOSED**

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

### G-11: No tamper-evident chain across backup versions — **CLOSED IN PRACTICE (2026-07, [ADR-0037](../adr/0037-backup-head-rollback-commitment.md))**

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

## Multi-device chat membership

### G-40: A second device never joins the account's existing conversations — **OPEN, closing under ADR-0050 (web built and live-tested 2026-09-15; natives not started)**

**Status 2026-09-15 (evening).** [ADR-0050](../adr/0050-per-device-conversation-leaves.md)
S1 core, S2 FFI, S3 discovery and S4 web are built; verified live on `test.lakko.me`
with three web clients: a device added through the ADR-0049 confirmation prompt was
admitted to the account's existing 1x1 chat by the confirming device and received the
messages sent after it joined. S5 (iOS + Android) built 2026-09-16, build and unit tests only. Still open under this gap: a live native and cross-platform run, the ADR-0049 confirmation prompt on the natives, incremental
contact sync between devices (the DeviceGroup sync kinds), channel subscriptions
following the device (blocked on the channel list surviving a reload at all), and the
removal fan-out unverified live. Closed later the same day: the scheduled re-admission
arm, the state bundle (contacts, pins, known groups) and the history bundle on the
confirmation route. The text below is the entry as minted that morning.

**Symptom.** Sign in on a second device by any route (QR pairing, the ADR-0049
confirmation prompt, the three-word vault, the full phrase, a passkey): the chat list
is empty, and a chat the first device holds never appears. Observed live on
`test.lakko.me` on 2026-09-15 with two Chrome profiles on one account.

**Cause.** Conversation groups are still on the ADR-0007 **Phase 1** model: one leaf
per *user*, signed by the AccountIdentity key. RFC 9420 section 5.3 requires every
leaf's signature key to be unique within a group, so a second device cannot be added
under the same key (OpenMLS `DuplicateSignatureKey`); discovery publishes one
KeyPackage per *account*, so a contact opening a chat adds one device; and pairing
hands over the DeviceGroup, the phrase seed and a plaintext history bundle only — the
bundle fills the message log, but the chat list is built from MLS group state the new
device does not hold. ADR-0007 named the fix — a per-device `DeviceIdentity` leaf in
every conversation group, bound to the account through the DeviceGroup — as
**Phase 2b** and only ever built Phase 2a (the DeviceGroup itself).

**Why it went unrecorded.** Every ADR that touched membership reported its own scope
as done, and the roadmap's Phase 2a line reads "multi-device sync via DeviceGroup
substrate", which the substrate satisfied. The missing product capability lived only
as an ADR-0007 open question and an ADR-0022 follow-up line, never as a gap. This
entry exists so that "multi-device" is not read as done again until it is.

**What closes it.** Phase 2b end to end: per-device conversation leaves with a
peer-verified device-to-account binding; per-device KeyPackages in discovery; the
existing device adding the new one to every conversation on pairing / confirmation
(ADR-0045 proposals where a role table exists); and every site that today equates a
leaf or the account key with a member (ADR-0022 roles, ADR-0023 authorship, ADR-0042
routing-secret authority, the ADR-0021 peer tag) resolving device to account. It is a
`lakko-mls` change and needs its own ADR plus property tests (invariant #3).

## Auth

### G-12: Session token validation is single-process in-memory — **CLOSED (was already addressed; text was stale)**

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

### G-13: WebAuthn passkey per-credential authentic-action attestation — **CLOSED**

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

### G-17: Frame-counter exhaustion path is test-exercised — **CLOSED**

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

### G-18: Overlap-window / mid-call rekey — **CLOSED (2026-07, [ADR-0029](../adr/0029-mid-call-key-rotation.md)); only the deployed-SFU live-rekey e2e deferred (like G-26)**

`GroupCallSession::drop_previous_call_key` is invoked by
the platform's own timer (iOS `Task.sleep` / Android
coroutine delay / web `setTimeout`). The Rust core stays
wall-clock-free per design (testability, no time-source
dependency). Each platform's timer correctness is reviewed
alongside the WebRTC pod integration in 3a9 / 3a10
followups; a "platform forgot to drop" would extend the
overlap window indefinitely and weaken seed-boundary
forward secrecy until the next rotation.

Mitigation: the platform's overlap timer is the same
trivial-to-audit `setTimeout(drop, 1000)` shape on every
client; failure mode is local-to-that-client (an attacker
cannot cause it remotely).

**All three platforms: helpers shipped.**

- Web — [`apps/web/src/calls/overlap-timer.ts`](../../apps/web/src/calls/overlap-timer.ts):
  `OverlapTimer` class wrapping `setTimeout`.
- iOS — [`apps/ios/Lakko/Calls/OverlapTimer.swift`](../../apps/ios/Lakko/Calls/OverlapTimer.swift):
  Final class with `Task.sleep` schedule, NSLock-guarded
  state, `@Sendable` callback.
- Android — [`apps/android/app/src/main/kotlin/me/lakko/app/calls/OverlapTimer.kt`](../../apps/android/app/src/main/kotlin/me/lakko/app/calls/OverlapTimer.kt):
  `kotlinx.coroutines.delay` schedule, `Mutex`-guarded
  state, scope-bound lifetime.

All three share the same API surface (`arm(callback)`,
`cancel()`, `state`/`isArmed`, three-state machine
`disarmed`/`armed`/`fired`), the same single-shot
"newer arm replaces prior" semantics, and the same
TTL bounds (`[50 ms, 10 s]`, default 1 s per ADR-0008).
Wiring them into a live call flow lands with the SFU
integration in 3b.

**Expiry decision moved into the core (Jun 2026).** The
per-platform timer no longer owns the 1-second policy:
`GroupCallSession::refresh_call_key_at(group, identity,
now_ms, overlap_ms)` stamps the retained previous key with
`now_ms + clamp(overlap_ms)` (bounds `OVERLAP_MIN_MS` ..
`OVERLAP_MAX_MS`, default `OVERLAP_DEFAULT_MS` = 1 s), and
`prune_expired_previous(now_ms)` drops it once the caller's
clock passes that instant (`has_live_previous(now_ms)`
reports the window). The core stays time-source-free — the
platform supplies only `now` — while the TTL bounds + the
1-second decision are owned + property-tested in one place
(5 tests in `group_call.rs`: retain-until-expiry, clamp,
no-clock-never-expires, second-rotation-resets-window,
saturating-add). The legacy no-clock `refresh_call_key` +
manual `drop_previous_call_key` remain for callers driving
their own timer. **The duplicated-logic part of G-18 is now
closed; the residual is just wiring the platforms onto
`refresh_call_key_at` + a clock-tick `prune_expired_previous`
when the live SFU call flow lands (3b).**

**CLOSED (2026-07, [ADR-0029](../adr/0029-mid-call-key-rotation.md)).** Mid-call
rekey is now wired end-to-end. A code reconciliation for this gap found the
overlap mechanism was **dormant** — no client instantiated `GroupCallSession`;
the clients seal/open at the raw-seed level, off-thread, without the MLS group.
So the fix was not "wire an existing timer" but to expose the overlap keyer at
the level the clients run at. The overlap logic was extracted into a seed-based
`lakko_calls::CallKeyRing` (current + previous key; `try_open_framed_with_overlap`
tries the current key then, during the window, the previous; `rekey_from_seed_at`
/ `prune_expired` / `has_live_previous`), which `GroupCallSession` **delegates**
to (its 5 property tests stay green, no behaviour change) and which is FFI-exposed
(uniffi + wasm) **and** surfaced in the lean `lakko-sframe-native` C ABI
(`lakko_sframe_call_key_ring_*`, Mutex-guarded) so the native SFrame media shim
can drive it. All three clients now rotate on a mid-call MLS epoch advance: the
chat receive loop re-derives `group.export_secret("lakko/call/v1", call_id, 32)`
at the new epoch and calls the live SFU call's `rekey(newSeed)`, which fans out to
every attached transform — the web `RTCRtpScriptTransform` worker (a `CallKeyRing`
per transform + a `rekey` postMessage + a self-prune timer), and the iOS / Android
native `LakkoFrameEncryptor::Rekey` (a seal-key swap — the send side keeps no
overlap) + `LakkoFrameDecryptor::Rekey` / `Prune` (the ring's overlap open, via
the C++ bridge). Fail-closed throughout: a re-derivation failure keeps the current
key, never plaintext (invariant #4). Verified headless on every stack — `cargo
test` (core `CallKeyRing` 6 property tests + the C-ABI overlap round-trip), the
uniffi (3) + wasm (3) overlap round-trips, the C++ bridge `test_mid_call_rekey_overlap`
(`just test-sframe-bridge`), web `vitest` (`call-key-ring.test.ts`), iOS
`xcodebuild`, Android `:app:compileDebugKotlin`. The only residual is a live
two-peer mid-call-rekey proof over a **deployed** SFU (a runtime gate, deferred
like G-26).

### G-19: SFU-side call-admission JWT validation — **CLOSED** (Rust SFU; LiveKit-fork plan superseded)

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

### G-20: Call recording is out of scope (deferred per ROADMAP 3c8)

If recording ever lands, the design is "recording client
joins as a regular participant with its own MLS leaf" — no
break to call-side invariants C-1..C-10 in
[`calls.md`](./calls.md). The recorded stream is
end-to-end encrypted just like any other consumer's view.

### G-21: Link-based call invites — **multi-joiner CLOSED end-to-end on web + iOS + Android ([ADR-0032](../adr/0032-multi-joiner-call-links.md): owner-founds + deterministic committer election + guest-account join); residuals are the account-creation-spam throttle (deferred backend arc) + a live deployed-SFU N-joiner/failover e2e (G-26-class)**

**Multi-joiner (ADR-0032) landed 2026-07** — the owner is the sole founder (no
founding race → the original two-groups bug closed by design); joiners mint
their own KeyPackage and are admitted by the `elect_committer`-designated
committer (owner-if-present-else-lowest-leaf, so failover + reclaim are
automatic + split-brain-free); the KeyPackage/Welcome ride the SFU `signal-relay`
in-band (live) or the stranger-inbox welcome-ferry (cold start); an anonymous
`/c/<slug>` visitor joins as a **guest** (a username-less OPAQUE account minted
via the audited PoW-gated `signup` path); an empty owner-founds call is dead
(~18 s timeout). Fail-closed throughout (no media before a valid Welcome + join;
a post-join owner-membership check refuses a redirect into an impostor group).
`lakko-security` **APPROVE-WITH-NOTES**. The single-joiner V1 note below is the
historical baseline this superseded.



`CallInviteCapability` embeds one one-time `KeyPackage`
(see C-26 in [`calls.md`](./calls.md)). The first joiner
to call `accept_call_invite` founds the call MLS group
with the owner already added; a second joiner using the
same capability would call `Group::create` on their own
side and end up in a **separate** MLS group — both would
derive the same `CallKey` seed (same `call_id` exporter
context) but the two groups wouldn't share an epoch, so
the call wouldn't connect.

**Server-side blob storage shipped in 3c3-server** —
`services/discovery /call-invite` + `/call-invite/{blob_id}`
+ `/call-invite/{blob_id}/consume` endpoints (mirror of
the contact-invite ones from 1.18d). Capability bytes
now live server-side keyed by SHA-256(bytes); the URL
becomes `https://lakko.me/c/<blob_id>` instead of
inline-encoded.

**Owner-signed revocation shipped in 3c3-revoke** —
`DELETE /discovery/call-invite/{blob_id}` mirrors
`invite::revoke`. Cross-kind isolation via
`CALL_INVITE_REVOKE_DOMAIN_V1` (distinct from the
contact-invite revoke domain) prevents signature replay
across the two ecosystems.

**Client-side helper trio shipped in 3c3-client** —
`uploadCallInvite(capabilityBytes)`,
`fetchCallInvite(blobId)`,
`consumeCallInvite(blobId, capabilityBytes,
joinerAccountId)`, and `revokeCallInvite(blobId,
signingSeed)` on all three clients
(`apps/web/src/discovery/client.ts`,
`apps/ios/Lakko/Discovery/DiscoveryClient.swift`,
`apps/android/.../discovery/DiscoveryClient.kt`). Wire
shapes reuse the contact-invite envelopes — the server
emits byte-identical JSON for both ledgers, only the URL
path + revoke signature domain separator differ.

**URL plumbing shipped in 3c3-url-helpers** — the
`<origin>/c/<slug>` URL form + parser on three platforms.
The byte-count discrimination contract is C-30 in
[`calls.md`](./calls.md): exactly 32 bytes ⇒ pointer
(SHA-256 `blob_id`); ≥ 400 bytes ⇒ inline (censored-region
fallback that skips the server round-trip); anything else
is rejected as `slugWrongLength`. 25 JUnit cases on Android
pin the discrimination + the constants-vs-web-side drift
sentinels (`CALL_INVITE_URL_PREFIX == "/c/"`,
`CALL_INVITE_BLOB_ID_LEN == 32`,
`CALL_INVITE_INLINE_MIN_LEN == 400`).

**Composer + acceptor orchestration shipped in
3c3-{composer,acceptor}-helper** — one-call `composeAndUpload(...)`
(5 steps: mint `call_id` → mint one-time KP →
`composeCallInviteCapability` via FFI → `uploadCallInvite`
via HTTP → build URL) and `acceptFromSlug(...)` (7 steps
collapsed into an 8-variant discriminated `Result`) on web
/ iOS / Android. The acceptor's defence-in-depth
re-verify (`decodeAndVerify` runs locally AFTER the server
already verified at upload time, so a malicious server
cannot substitute bytes) and `404 → notFound` folding
(denies status-code probing of the `blob_id` space) are
pinned by the C-31 invariant. The acceptor's
`Result.accepted` exposes the 32-byte `callKeySeed` so UI
seals the joiner's first SFrame frame before the owner has
joined — no second FFI round-trip needed.

**Welcome-ferry wire path shipped in
3c3-welcome-ferry** — the joiner-side `acceptCallInvite`
flow's `welcomeForOwner` bytes now reuse the existing
`ContactRequest` envelope transport (HPKE-sealed to the
owner's identity key, routed via the
`StrangerInbox::derive(owner_account_id)` pseudonym).
Joiner-side: wraps the Welcome bytes with the 22-byte
domain separator
`CALL_WELCOME_ENVELOPE_DOMAIN_V1 = b"lakko/call-welcome/v1\x00"`
via [`lakko_contacts::call_welcome::wrap_call_welcome`](../../crates/lakko-contacts/src/call_welcome.rs)
before HPKE-sealing through the standard
`ContactRequest.compose(...)` path. Owner-side: polling
loop calls
[`try_unwrap_call_welcome(plaintext)`](../../crates/lakko-contacts/src/call_welcome.rs)
on every decrypted envelope and dispatches on a
three-way discriminator — `Some(welcome)` → drive
`Group::join_from_welcome(welcome)` and enter the call
group; `None` → render as a plain intro message
(ADR-0005 scenario A); `Err(TruncatedPayload)` → log +
drop a malformed envelope. The separator is asserted
distinct from all sibling domain prefixes (contact-invite
revoke, call-invite revoke, joiner-tag salt) by the
`separator_distinct_from_contact_invite_domains` test.
The reuse means **zero new server surface** — the
existing PoW + Privacy Pass anti-abuse gate on
`ContactRequest` ingress carries over for free, and the
server still sees only `(pseudonym → opaque ciphertext)`.
FFI exposure shipped on both wasm-bindgen
(`wrapCallWelcome` / `tryUnwrapCallWelcome` /
`callWelcomeMinPayloadLen`) and UniFFI surfaces. See
**C-32** in [`calls.md`](./calls.md) for the full
invariant set + 9 property tests in
`lakko_contacts::call_welcome::tests`.

**Platform-side wiring still pending** — the wrap /
unwrap helpers are in place but the platform polling
loops on web / iOS / Android still need to (a) call
`tryUnwrapCallWelcome` on every decrypted
`ContactRequest` they pull from the stranger inbox, (b)
dispatch a Welcome to `Group::join_from_welcome` instead
of the intro-message renderer, (c) transition the call
UI from "waiting for owner" to "in-call" once the
Welcome lands. This is straightforward glue work
deferred to the platform-UI sprint; the cryptographic
contract it depends on is fixed.

**Subsequent-joiner KP-forward wire path shipped in
3c3-kp-forward** — sibling envelope of call-welcome,
under a distinct domain separator
`CALL_KP_FORWARD_ENVELOPE_DOMAIN_V1 = b"lakko/call-keypackage/v1\x00"`.
Subsequent joiners wrap their fresh `KeyPackage` via
[`wrap_call_kp_forward`](../../crates/lakko-contacts/src/call_kp_forward.rs)
and ferry it through the same `ContactRequest` envelope
transport to the call group's current committer. The
committer's polling loop dispatches in order:
`try_unwrap_call_welcome` → `try_unwrap_call_kp_forward` →
intro-message fallthrough. The no-cross-dispatch property
([`welcome_and_kp_forward_round_trips_do_not_cross_dispatch`](../../crates/lakko-contacts/src/call_kp_forward.rs))
guarantees no payload can match two branches at once. See
**C-33** in [`calls.md`](./calls.md) for the full
invariant set + 10 + 5 tests.

**Owner-side add-member orchestration still pending** —
the committer reads the unwrapped KP and runs MLS
`Group::add_member_from_wire(kp_bytes)` + commits, then
ferries the resulting Welcome back to the subsequent
joiner via [`wrap_call_welcome`](../../crates/lakko-contacts/src/call_welcome.rs)
(same C-32 envelope). The MLS operations + the platform-
side polling-loop wiring is straightforward glue
deferred to the platform-UI sprint; the cryptographic
contract is now fully fixed for both halves of the
multi-party handshake.

**Owner-handover protocol shipped in 3c3-handover** —
the committer-leaves scenario now has a pinned policy:
the capability's `owner_account_id` designates the
**owner as the primary committer-of-record**. When the
first joiner disconnects, the owner drives the Remove
commit for the stale leaf (OpenMLS does not permit
self-removal via `remove_members`, and a disconnected
client can't drive `leave_group_via_self_remove`
anyway). Subsequent KP-forwards then route to the owner
as the new committer. Across the leave + add commits
the MLS exporter produces a fresh CallKey seed each
epoch (forward secrecy assertion). The integration test
[`link_call_owner_handover_after_committer_leaves`](../../crates/lakko-calls/src/link_invite.rs)
exercises the full 4-phase sequence (A↔B↔C baseline →
Alice removes Bob → Dave joins via Alice → all three
at same post-handover epoch + byte-identical CallKey
seeds + pre-leave seed differs from post-handover
seed). See **C-36** in [`calls.md`](./calls.md) for the
full invariant set.

The owner-handover test surfaced + fixed a load-bearing
MLS bug: `Group::join_from_welcome` was using
`MlsGroupJoinConfig::default()` which set
`use_ratchet_tree_extension = false` on joined groups,
so a member who joined via Welcome and later committed
add (the exact owner-handover scenario) produced a
Welcome that didn't carry the ratchet tree → the new
joiner failed with `MissingRatchetTree`. The one-line
fix in `lakko-mls::Group::join_from_welcome` flips the
flag on, matching the founder's `MlsGroupCreateConfig`,
so a chain of welcome-then-add-then-welcome propagation
now survives any number of hops.

Operational impact: V1 1:1 AND multi-party (3+) link
calls have a **complete protocol-level wire path** in
all directions including the committer-leaves
handover. The full cryptographic state machine is
backed by named integration tests (C-35 happy path,
C-36 handover path). What remains for V1 is purely
platform-UI glue: composer view, `/c/<slug>` landing
page, polling-loop integration with
`dispatch_incoming_envelope`. The cryptographic
contracts the UI work depends on are all fixed and
fuzz-covered.

### G-22: Per-joiner dedup for call invites — **fully CLOSED ([ADR-0032](../adr/0032-multi-joiner-call-links.md)): committer-side per-signature-key admission dedup on all 3 clients via the `keyPackageSignatureKey` FFI helper**

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

### G-31: Cross-stream SFrame AEAD nonce reuse (multiple streams under one leaf) — **CLOSED (2026-07, [ADR-0035](../adr/0035-sframe-per-stream-kid-key.md); [ADR-0034](../adr/0034-sframe-per-media-stream-key.md) was an incomplete first attempt, superseded)**

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

### G-27: Channel PublisherGroup — **CLOSED** — all three platforms wired (G4-2-mls-publisher + G-27 iOS / Android slice) + persistence + rotation landed (G4-2-mls-persistence)

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

### G-28: SFrame on-device wiring — CLOSED (May 2026) via the G3-sframe-native arc (N-1..N-6)

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

## Out-of-protocol

### G-14: Hostile-OS / hostile-browser threats

No app-level integrity attestation. We rely on the
platform's standard sandbox + the user's awareness. Phase 5
adds App Attest / Play Integrity / WebAuthn-attestation to
the signup / login round-trip.

### G-15: Side-channel + physical attacks

Out of scope for this audit pass. Mitigated by relying on
the platform's constant-time crypto primitives (`subtle`
crate, OpenMLS's vetted implementations). No hand-rolled
crypto in the codebase — the `lakko-crypto` crate is
plumbing only.

### G-16: Quantum adversaries

MLS suites pin classical curves today. PQ migration
follows the WG.

### G-26: RFC 9605 §4.3 SFrame wire header + framed pipeline + cross-platform AAD; §4.3-conformant, only live deployed-SFU RTP interop pending

**Update (2026-07) — the §4.3 rewrite landed; the header is now RFC 9605 §4.3
wire-compatible.** An earlier revision of `SframeHeader::encode` used a
Lakko-custom first-byte layout (`R | LEN | X | KID` with an always-trailing CTR)
that a 2026-06-30 finding (`lakko-security` APPROVE-WITH-NOTES, verified against
the RFC + its Appendix C.1 vectors) showed was NOT RFC 9605 §4.3. The maintainer
took the §4.3 rewrite: `encode`/`decode` now pack the config byte as the §4.3
`X | KKK | Y | CCC` with inline `KID<=7` / `CTR<=7`, **proven byte-for-byte
against RFC 9605 Appendix C.1's published vectors**
(`header.rs::rfc_9605_appendix_c1_header_vectors`), so `(kid=0, ctr=0)` is now
the single byte `00` exactly as the RFC specifies. Because the header IS the
AEAD AAD (ADR-0013) and it is now spec-conformant, a Lakko frame is
interoperable with any other RFC 9605 implementation (mlspp, Cisco libsframe, a
browser-native SFrame transform) that shares the key + nonce derivation —
external-stack interop is no longer broken by design. The golden vector was
re-pinned + re-verified firsthand across all six stacks (Rust core, the
`lakko-sframe-native` C ABI, the C++ bridge, web wasm, plus the iOS/Android
golden pins run in CI). The change carried no cryptographic risk (encode +
decode flip together; the nonce derives from the counter, not the header
layout). Everything below about cross-RUNTIME byte interop remains true — the
same golden vector, now in the §4.3 encoding. The one residual is a live
cross-platform / external-stack RTP round-trip over a DEPLOYED SFU (a
verification gate, not a design gap).

**Status (2026-06-02):** the framed pipeline + cross-platform AAD landed; the
Lakko-internal cross-runtime interop is proven (golden vector); the live
deployed-SFU RTP smoke is the only deploy-gated residual. Progress since
the 2026-05-25 entry:

1. **Wire serializer.**
   [`crates/lakko-calls/src/header.rs`](../../crates/lakko-calls/src/header.rs)
   ships `SframeHeader::encode` / `SframeHeader::decode`
   (RFC 9605 §4.3: config byte `X | KKK | Y | CCC` + inline
   `KID<=7` / `CTR<=7`; round trip + pinned wire layouts +
   truncation handling + bit-packing first-byte invariants +
   the RFC 9605 Appendix C.1 published-vector conformance
   test). See the Update above.
2. **Framed seal/open pipeline — DONE.**
   [`lakko_calls::sframe`](../../crates/lakko-calls/src/sframe.rs)
   now exposes `ParticipantKey::seal_framed` (peeks the
   counter, builds the header, seals with the header as
   AEAD AAD, returns `header || ciphertext`) and
   `open_framed` (decodes the header, binds the EXACT
   on-wire header bytes as AAD, checks `kid == sender_leaf`,
   opens). 7 property tests added on top of the prior
   suite. The SFU reads the KID off the wire to route by
   participant without touching the ciphertext.
3. **Cross-platform AAD contract — [ADR-0013](../adr/0013-sframe-cross-platform-aad.md).**
   The AEAD AAD is the SFrame wire header (KID + CTR), NOT
   any host RTP `additional_data`. This is the property
   that lets a frame sealed on one platform open on
   another: the host metadata is not reconstructible
   across stacks, so it cannot be in the authenticated set.
4. **All three client surfaces consume the framed path.**
   The flat C ABI (`lakko-sframe-native`:
   `lakko_sframe_seal_framed` / `_open_framed`), UniFFI
   (`ParticipantKey.seal_framed` / `open_framed`), and
   wasm-bindgen (`sealFramed` / `openFramed`) all expose
   it. The native libwebrtc C++ bridge
   (`apps/shared/sframe-bridge`) calls `seal_framed` /
   `open_framed` directly (host `additional_data`
   intentionally unbound); its 8-assertion host test
   (`just test-sframe-bridge`) proves both that a
   mismatched host AAD does NOT fail decryption and that a
   tampered header DOES. The browser side is the
   `RTCRtpScriptTransform` worker
   (`apps/web/src/calls/sframe-transform.ts` +
   `sframe-transform-worker.ts`) calling `sealFramed` /
   `openFramed` inside a module worker.

**What is still open — live cross-platform RTP verification.**
The byte-level interop is asserted by construction (the
header is platform-independent; native + web call the same
`lakko-calls` core through the same framed API) and by the
native host test. What has NOT been observed firsthand is a
real media frame sealed on one platform (e.g. a Chrome
`RTCRtpScriptTransform` sender) decrypting on another (e.g.
an iOS / Android libwebrtc receiver) over a live SFU
forwarding path. That requires two real endpoints plus the
SFU in the middle and cannot be done in a headless build
environment.

**Narrowed (2026-06-12).** The "does an SFrame frame survive
the live SFU at all?" question is now answered firsthand in a
headless test:
[`sframe_sealed_frame_round_trips_through_the_live_sfu`](../../services/sfu/tests/synthetic_client_e2e.rs)
seals a frame with the real `lakko_calls::sframe`
`seal_framed`, pushes it through the production
`spawn_media_task` over a real `str0m` ICE + DTLS handshake +
the cross-fanout renegotiation, and the subscriber
`open_framed`s the forwarded bytes back to the exact
plaintext — so the RFC 9605 §4.3 header (the AAD) and the
ciphertext provably survive the SFU forward intact, and the
SFU stays SFrame-opaque (`lakko-calls` is a **dev-only** dep
of `services/sfu`; the production binary never links it,
preserving C-39).

**Effectively closed (2026-06-12) — cross-runtime byte interop
proven by golden vector.** The cross-*runtime* leg no longer
rests on "by construction": a single canonical SFrame wire —
sealing `(seed=[0x42;32], leaf=3, plaintext="lakko sframe
golden vector v1")` on a fresh `ParticipantKey` (counter 0) —
is now pinned and asserted on ALL FOUR stacks, each of which
both SEALS to the identical bytes and OPENS them back to the
plaintext: the Rust core
([`sframe.rs::golden_vector_*`](../../crates/lakko-calls/src/sframe.rs)),
the wasm/web `RTCRtpScriptTransform` stack
([`sframe-golden.test.ts`](../../apps/web/src/calls/sframe-golden.test.ts),
vitest), the iOS UniFFI stack
([`SFrameGoldenTests.swift`](../../apps/ios/LakkoTests/SFrameGoldenTests.swift)),
and the Android UniFFI stack
([`SframeGoldenTest.kt`](../../apps/android/app/src/androidTest/kotlin/me/lakko/app/calls/SframeGoldenTest.kt)).
The Rust, web, and iOS golden tests were verified firsthand
(iOS live on a simulator); the Android instrumented test
compiles and runs in CI (`g3-4-mobile.yml` →
`connectedDebugAndroidTest`); the Rust + web tests gate every
CI run (`cargo test --workspace` + vitest). Because every stack
produces and consumes the byte-identical wire, a frame sealed
on any stack opens on any other — the cross-runtime interop is
now a *proven equality*, not an assumption. Composed with the
live-SFU verbatim-forwarding test above, the
sealed-on-web → relayed-by-SFU → opened-on-native path is
established by correctness: the SFU forwards the wire
unchanged, and that exact wire is what every native stack
opens.

**Composition now observed in one headless test (2026-06-15).**
The "two already-proven facts" no longer need to be composed
mentally: [`golden_vector_round_trips_through_the_live_sfu`](../../services/sfu/tests/synthetic_client_e2e.rs)
seals the EXACT canonical golden vector (`seed=[0x42;32]`,
`leaf=3`, `"lakko sframe golden vector v1"`), asserts the sealed
wire equals the byte-pinned `GOLDEN_WIRE_HEX` that the Rust /
web / iOS / Android golden tests all pin, pushes THOSE bytes
through the production `spawn_media_task` over a real `str0m`
ICE + DTLS + cross-fanout renegotiation, and the subscriber
`open_framed`s the forwarded bytes back to the golden plaintext.
So the bytes a real web / iOS / Android client emits ARE the
bytes observed to cross the live SFU intact — the composition is
a single firsthand-verified fact, not a mental join. Verified
firsthand (`cargo test -p lakko-svc-sfu --test
synthetic_client_e2e` → 5 passed) and gates every CI run.

**Native C ABI golden pin added (2026-06-17).** The flat
`lakko-sframe-native` `extern "C"` surface — the path the iOS
Obj-C++ shim + Android JNI module actually call before handing
bytes to libwebrtc's `FrameEncryptorInterface` /
`FrameDecryptorInterface` — is now itself pinned to the canonical
golden in
[`crates/lakko-sframe-native/tests/golden_cross_stack.rs`](../../crates/lakko-sframe-native/tests/golden_cross_stack.rs):
`lakko_sframe_seal_framed` over the golden seed / leaf / plaintext
produces the EXACT `GOLDEN_WIRE_HEX`, and `lakko_sframe_open_framed`
recovers the golden plaintext from that wire. Previously the C ABI
was only opacity-tested with arbitrary per-test seeds
(`sfu_opacity_e2e.rs`); the cross-stack golden was pinned on the
core + web/wasm + iOS/Android UniFFI surfaces but NOT on the C ABI
the native media shims wrap. This closes that last in-codebase
coverage gap — core, web (wasm), UniFFI, the native C ABI, and the
live `str0m` SFU round-trip now ALL agree on the one canonical wire.
Verified firsthand (`cargo test -p lakko-sframe-native --test
golden_cross_stack` → 3 passed).

The C++ bridge ONE LAYER up is now golden-pinned too: a
`test_cross_stack_golden_vector` assertion in
[`apps/shared/sframe-bridge/test_bridge.cc`](../../apps/shared/sframe-bridge/test_bridge.cc)
drives the actual `LakkoFrameEncryptor::Encrypt` /
`LakkoFrameDecryptor::Decrypt` (the concrete `webrtc::FrameEncryptorInterface`
/ `FrameDecryptorInterface` impls the iOS Obj-C++ shim + Android JNI module
wrap before handing bytes to libwebrtc) over the golden vector — `Encrypt`
produces the EXACT `GOLDEN_WIRE_HEX` (even with a non-empty host
`additional_data`, re-proving ADR-0013 host-AAD-independence) and `Decrypt`
recovers the golden plaintext. Verified firsthand (`just test-sframe-bridge`
→ 9 assertions pass). So every layer of the native media stack — the C ABI,
the C++ bridge, and the webrtc-interface impls — is proven byte-identical to
web / iOS-Android-UniFFI / the core, all the way down to the libwebrtc handoff.

The only residual is a single monolithic real-Chrome ⊕
real-native ⊕ deployed-SFU smoke run — redundant for
correctness (it would only re-observe the same composition with
real codec pipelines), and a deploy/CI-execution concern, not a
development one. The web multi-party session that drives the
worker (`apps/web/src/calls/sfu-call-session.ts` — composes
the `lakko-sfu-v1` signaling client + the call-admission JWT
+ one `RTCPeerConnection` to the SFU + `attachSenderSFrame` /
`attachReceiverSFrame` on every track + SFU-initiated
renegotiation) is now wired into the chat UI: a group chat
(>2 members) routes the call button through
`createSfuCallUi`, which drives the shared call-UI store
roster, plays remote audio, and renders per-tile remote
video, while 1×1 chats stay on the P2P path. The MSID
stream-id → source-peer correlation the video tiles need is
pinned by construction — the SFU sets the downstream MSID
stream-id to the source peer id (`add_media(..,
Some(source_peer.to_string()), ..)` in
`services/sfu/src/media.rs`), and that string is the same
`PeerId` the signaling roster carries, so
`ev.streams[0].id` maps straight onto the roster tile. The
**native** iOS + Android clients are now wired to the same
extent (SFU multi-party arc, complete): `SfuSignalingClient`
(WS + typed `lakko_sfu_proto` frames) + `SfuCallSession`
(one `RTCPeerConnection`/`PeerConnection` to the SFU,
`armSFrameEncryptors` on every local sender before the offer,
`armSFrameDecryptor` on every remote receiver, SFU-initiated
renegotiation) + the `SfuCallUi` adapter driving the shared
`CallUiState` + the chat-header trigger/presentation. Both
publish SFrame-sealed video when the call is started with the
video-call button (the local camera is captured, the video
track added before `armSFrameEncryptors` + the offer, so the
video sender is sealed alongside audio). All three platforms
therefore reach the SFU through the identical framed
`seal_framed` / `open_framed` core. What remains is only the
live cross-platform RTP verification above (a real frame
round-tripping web↔native through a deployed SFU) — a
verification step, not a missing mechanism.

**Why this isn't a security gap today.** The keying math +
the AAD set + the wire layout are all RFC 9605 conformant
and shared across platforms by construction. The residual
item is a verification step (does a live cross-stack frame
round-trip?), not a missing mechanism. The fail-closed
posture holds throughout: a frame that fails to
parse / authenticate is dropped, never forwarded as
plaintext (invariant #4), on every surface.

## Maintenance + reconciliation

### G-29: in-code completion markers swept + reconciled; residual genuinely-open items

**Reconciliation pass (2026-06-09).** The codebase's in-code "TODO / not yet
wired / for now / lands in X / deferred" annotations had drifted badly out of
date — a four-area sweep (Rust crates + services, web, iOS, Android) found
that the large majority described work that had **already landed**, and
corrected **~45 stale comments** to match the code (e.g. iOS/Android/web call
surfaces claiming "audio-only / SFrame deferred / video lands later" when
video + mandatory SFrame are fully wired; "passkey sign-in not wired" when it
is; invite short-pointer / max-uses / KP-forward "deferred" when the discovery
endpoints + crate paths exist; the Android recover snapshot "only
re-published" when it is applied to `IdentitySnapshotStore`). The in-code
annotations now reflect reality; this entry records what genuinely **remains**
after the sweep.

**Already tracked elsewhere (cross-references, not new):** identity/invite
lookups are TOFU pending the deployed transparency service (G-4 / ADR-0016);
external-stack SFrame RTP interop unverified (G-26); single-link many-stranger
call rendezvous absent — multi-party works via owner KP-forward (G-21); the DS has no
auth/PoW (pseudonym-unguessability is the Phase-1 gate).

**Genuinely open, security-relevant:**

- **Channel subscriber secret moved to hardware-backed storage (G-29,
  Jun 2026).** The subscriber-side X25519 secret half (it unwraps the
  per-subscriber HPKE seed envelopes) now persists encrypted at rest on both
  native platforms (invariant #7). **iOS:** moved off `UserDefaults` plaintext
  into the Keychain (`ChannelSubscriberKeyStore`, `AfterFirstUnlockThisDeviceOnly`)
  with a one-time migration that scrubs the legacy plaintext copy. **Android:**
  the subscriber-key persistence (previously a `SharedPreferences` path that
  was never actually wired — `ChannelsUiState` was constructed with no prefs,
  so the keys were in-memory only and lost on relaunch) is now wired to an
  `EncryptedSharedPreferences` (Keystore AES-256-GCM, `ChannelKeyStore`), so the
  secret both survives a relaunch and is encrypted at rest. No Android migration
  is needed (no prior on-disk copy existed). Two prior claims on this surface were
  stale and are corrected: the publisher `signingKeySeed` is **in-memory only**
  (not persisted, so there is no at-rest exposure — persisting it to the
  Keychain for cross-relaunch publisher continuity is a separate functional
  follow-up), and the publisher content-key seed **is** derived from the
  PublisherGroup MLS exporter (`publisherGroupOpenOrCreate` + `channelSeed`,
  G4-2-mls-persistence), not a demo value. `xcodebuild` green.

**Genuinely open, UX / feature limitations (not correctness or security
breaks):**

- **Cross-device recover via discovery — wired (stale-doc reconcile,
  Jun 2026).** This entry described `recover()` as requiring a cached
  `account_id`, but the discovery lookup is in fact wired on all three
  platforms (verified firsthand): web `AuthContext.recover` (the optional
  `username` arg → `discovery.lookup` on a cache miss), iOS
  `AuthRepository.recover(mnemonicPhrase:newPassword:username:)`
  (`DiscoveryClient.lookup`), and Android `AuthRepository.recover`
  (`discoveryClientFactory().lookup`). All three resolve the `account_id` the
  same way `login()` does — a local `(accountId, username)` cache hit wins,
  else a `discovery.lookup(username)` round-trip — so a FRESH device can
  recover-from-words given the username + phrase; the `username` arg is
  required only when no account is cached. A username miss collapses into the
  same error as a bad phrase (no username-space probing). The live recovery
  `recovery_kt_e2e.rs` exercises the recovery pipeline, and the resolution
  branch is now factored into a pure `resolveRecoverAccount` helper with a
  6-case unit test (cache-hit / matching-username / fresh-device lookup /
  username-mismatch / unregistered-username / no-account) on **all three
  platforms** — web `apps/web/src/auth/recover-resolution.{ts,test.ts}`, iOS
  `apps/ios/Lakko/Auth/RecoverResolution.swift` + `LakkoTests/RecoverResolutionTests.swift`,
  Android `.../auth/RecoverResolution.kt` + `test/.../RecoverResolutionTest.kt`
  — locking the cross-device behaviour so it can't silently regress (each
  `recover()` is a behaviour-identical refactor onto the shared helper; 6/6
  green on every platform).
- **Web 1×1 video calls landed (Jun 2026).** The web P2P 1×1 path now
  captures the local camera (a "Video" call button dials `video:true`),
  renders a mirrored self-preview + the remote video tile, and toggles the
  camera in-call (`CallSession.setCameraEnabled` + the `onLocalStream`
  callback; `Call.tsx` `localStream` prop + `tileStream`). tsc + vitest (207)
  + vite build green; live two-peer video needs a browser with cameras (a
  runtime gate, like G-26). **In-call device picker — wired to real enumeration
  (Jun 2026):** `Call.tsx`'s settings panel now pulls
  `navigator.mediaDevices.enumerateDevices()` into `callActions.setAvailableDevices`
  on panel-open + on `devicechange` (the picker already rendered the list +
  blank-label fallback + empty-state hint; nothing fed it before).
  **Applying the selection — wired (Jun 2026):** the 1×1 `CallSession` honours
  the picked mic/camera at capture time (`CallMedia.audioDeviceId`/`videoDeviceId`
  → `getUserMedia({ deviceId: { exact } })`) and
  `switchAudioInput`/`switchVideoInput` re-capture + `replaceTrack` on the live
  sender for a mid-call change (no renegotiation, preserves the mute state);
  `Chat.tsx` threads the selection into the outgoing dial + a debounced effect
  applies a mid-call picker change. **The SFU multi-party group call honours +
  switches too (Jun 2026):** `SfuCallSession` honours the picked mic at publish
  (`SfuCallMedia.audioDeviceId`) + `switchAudioInput`/`switchVideoInput`
  `replaceTrack` on the live sender — the sender keeps its SFrame transform, so
  media stays end-to-end encrypted through the SFU (invariant #4, no re-attach);
  `trackConstraint` is shared from the 1×1 `CallSession`, the `SfuCallUiHandle`
  exposes the two methods, and the same Chat.tsx switch effect drives the SFU
  handle (group calls are audio-only for now, so the camera switch is a
  graceful no-op). Residual: the answerer captures the OS default at accept
  (the switch effect then applies any pre-selected device once the call is
  live). tsc + vitest (213) + vite build green.
- **iOS contact inbox — background refresh wired (Jun 2026).** A
  `BGAppRefreshTask` (`ContactBackgroundRefresh`, registered on the scene via
  `.backgroundTask(.appRefresh(_:))` in `LakkoApp`, scheduled on the
  foreground→background transition + re-chained after each run) now wakes the
  app opportunistically to run one quick `refreshInbox()` fetch, so a request
  that arrives while backgrounded is picked up without a manual reopen.
  `Info.plist` declares the `fetch` `UIBackgroundMode` +
  `BGTaskSchedulerPermittedIdentifiers`. `xcodebuild` green. This is
  opportunistic (iOS decides if/when to run it); an APNs `content-available`
  silent-push wake for *immediate* surfacing remains a follow-up — it needs the
  push gateway to emit a silent push on inbox arrival.
- **Android QR pairing scanner — resumable after a non-Lakko QR (CLOSED,
  Jun 2026).** The `QrScannerComposable` latch changed from a one-shot
  `AtomicBoolean hasDetected` to an `AtomicReference<String?> lastHandledRaw`:
  it now fires `onDetected` once per *distinct* decoded value, so the same
  non-Lakko QR held in frame doesn't busy-loop but a *different* code (the user
  panning onto the real pairing QR) fires immediately — no screen re-mount.
  `DevicesScreen` no longer dead-ends to `ScanUi.Error` on a non-pairing QR; it
  stays in `Scanning` and shows a transient inline hint. `:app:compileDebugKotlin`
  green.
- **iOS + Android accepted-contacts list is in-memory only** (rebuilt by
  re-polling the stranger inbox from cursor 0 on launch). The backup
  restore-merge of contacts is therefore an optimization, not a correctness
  gap. (This entry also said the MLS whole-storage snapshot is restored on
  recover on both; since 2026-10-02 the backup carries no MLS state and
  neither client loads one, see `backup.md`.)

**Genuinely open, maintenance / observability:**

- **CLOSED (Jun 2026).** The PoW challenge-issue/verify/consume/prune logic
  (previously duplicated near-verbatim across
  `services/{auth,discovery,push-gw}/src/pow.rs` — a drift risk where a
  security fix to one copy could miss the others) now lives once in the new
  `lakko-pow-server` crate (distinct from `lakko-pow`, the wasm-portable client
  *solver*). The three diverged only in the doc comments + the effort env-var
  name; the crate parameterises the latter (`configured_effort_bits(var_name)`)
  and each service's `src/pow.rs` is reduced to a thin re-export + the axum
  handler that needs its `AppState`. `equix` is dropped as a direct dep of
  discovery + push-gw (now transitive via the crate; auth keeps it for its
  `#[cfg(test)]` solver helpers). Verified: `cargo test -p lakko-pow-server`
  (5 passed), clippy `-D warnings` clean on the crate + the 3 services, their
  unit suites green, `just audit` clean.
- **CLOSED (Jun 2026).** `lakko_sfu_signaling_frames_total` is now labelled by
  `direction` (inbound = received from a peer / outbound = broadcast to peers)
  + `kind` (the typed frame kind, or `invalid` for an undecodable inbound
  frame). `lakko-sfu-proto` gained a `kind_str()` accessor on `ServerFrame` +
  `ClientFrame` (the single source of truth for the kebab tag, with unit tests
  asserting it never drifts from the serde `kind` tag); `signaling.rs` labels
  the inbound counter by the parsed `ClientFrame` kind, `router.rs` the
  outbound broadcast counter by the `ServerFrame` kind. The unlabelled
  zero-init was dropped (like the labelled `MEDIA_FRAMES_DROPPED_TOTAL`, it
  first appears once traffic flows). `cargo test -p lakko-sfu-proto`
  (21 passed) + `-p lakko-svc-sfu` green; clippy `-D warnings` clean.

## Key transparency

### G-30: KT-log (S1-S3) deployed-verifier scope + append-binding trust + equivocation residual — **#2 closed (ADR-0019) + genesis squatting PREVENTED (ADR-0036, flag-gated), #1 closed (ADR-0020)**

**Status (2026-06-12):** the key-transparency log is built end-to-end —
`services/transparency` (operator-signed append-only CONIKS log: S1), discovery
logging every identity-key change (S2) + a reconciliation sweep (S2.5), and all
three clients verifying a fetched contact's published identity against the log
(S3: `verify_log_inclusion` FFI + `TransparencyVerify` on web/iOS/Android).
`lakko-security` reviewed the whole arc: **APPROVE-WITH-NOTES**. Three residuals
are recorded here so a future reader does not over-trust the deployed check:

1. **The deployed verifier is NOT the chain verifier — no equivocation
   resistance.** S3 wires the single-client `lakko_transparency::verify_inclusion`
   path (latest entry + inclusion proof + a monotone last-seen `log_size`
   no-rollback check), NOT `verify_supersession_chain`. A malicious operator
   (it holds the signing key) can sign two different valid roots and show each
   to a different victim (split-view / equivocation); each client sees a
   monotone, self-consistent view and CANNOT detect it. Detecting split-view
   needs gossip (clients cross-checking roots) or witness co-signing — **not
   built**. What the deployed design DOES defend: a server that strips / omits /
   rolls back (→ `absent` / `unreachable` surfaced, `rollback` hard-blocked) and
   a serving-tier (CDN / proxy / DB) key swap (defeated by the build-time
   operator-key pin — the client never trusts `GET /operator-key` for
   verification). The operator pin is a real reduction (compromised *serving*
   tier ≠ compromised *signing* tier), not theatre, even before gossip. The
   signing seed lives in the service env, so a host compromise forges roots
   freely — the same threat boundary as every other server-held signing key
   (call-JWT ADR-0008, device-attest ADR-0015, OPAQUE setup). Cross-ref
   ADR-0016 §equivocation.

2. **Append-binding trust — the log is only as honest as discovery + the append
   secret.** The transparency service sees only `username_hash =
   BLAKE3("lakko/transparency/account/v1" || account_id)`, never the
   `account_id`, so it cannot verify the `account_id ↔ username_hash` binding.
   Anyone holding `LAKKO_TRANSPARENCY_APPEND_SECRET` (the discovery service, or
   a leaked server-to-server secret) can append a leaf for any account-key with
   any pubkey, carrying a valid inclusion proof under the operator-signed root.
   This is NOT a silent key substitution — a forged leaf still has to pass the
   client's fail-closed ADR-0016 supersession gate (runs FIRST, refuses a
   changed key with no old-key-signed proof), so the worst it achieves is
   log-poisoning / a confusing `mismatch` surfaced to the user. Closing it fully
   needs a client-supplied signature by the account-identity key over its own
   `(username_hash, pubkey, version)`, so the service / auditors verify the
   binding without trusting discovery — **future work**.

3. **Mismatch / absent posture is surface-not-block (adjudicated correct, for
   now).** A `mismatch` (the log's latest pubkey ≠ the served pubkey) or
   `absent` (no log entry) is recorded + surfaced but does NOT block the
   contact; only `rollback` hard-blocks. Correct because the fail-closed
   supersession/pin gate (ADR-0016) is the PRIMARY trust and runs first, and
   because discovery's append is best-effort fail-open (S2), so a legitimate
   rotation reaching discovery before the S2.5 reconciler runs produces a
   transient `mismatch`/`absent`; hard-blocking would break legit contacts
   during the consistency window. The condition to flip `mismatch` (not
   `absent`) to fail-closed: the append moves onto discovery's **transactional
   critical path** (synchronous — "logged" becomes an invariant of "published",
   collapsing the consistency window) plus a bounded staleness SLA.

A `verify_log_inclusion` fuzz target was added
(`crates/lakko-transparency/fuzz/fuzz_targets/verify_log_inclusion.rs`) closing
the "new attacker-touchable verify surface → fuzz target" rule.

**Update — G-30 #2 closed by ADR-0019 (client-authorised log leaf), 2026-06-12.**
Finding 2 is CLOSED and finding 1's rotation case is materially addressed:

- **Rotations are now chain-verified (finding 1, rotation case).**
  `verify_supersession_chain` is FFI-exposed (UniFFI + wasm) and wired into all
  three clients' gate 2 (`verifyRotationChain`): on a key change the rotation is
  bound to a SINGLE committed log version under a no-rollback signed root
  (operator-root sig → no-rollback → inclusion → username match → strictly-newer
  version → old-key supersession proof → leaf-pubkey == proof key). This makes
  leaked-old-key equivocation **detectable** (two distinct successors cannot both
  be log-confirmed at the same version → the second peer to fetch sees a
  version-floor / inclusion failure → `mismatch`). It is detection (depends on
  ≥1 honest fetch + the OOB safety-number backstop), NOT full split-view
  resistance — operator equivocation across DIFFERENT signed roots needs
  witness co-signing (finding 1, **now closed by ADR-0020** — see the update
  below).

- **Append is now client-authorised (finding 2, CLOSED).** Every new
  `(account_key → pubkey)` binding must carry a `leaf_auth`: a genesis / reset
  self-signature by the leaf's own key (domain `lakko/transparency/leaf-genesis/v1`),
  or a rotation supersession proof signed by the previous key. The transparency
  `append` handler verifies it under the append lock and REJECTS an invalid one
  (403); `LAKKO_TRANSPARENCY_REQUIRE_LEAF_AUTH` (default on) also refuses an
  ABSENT one. Discovery threads `leaf_auth` opaquely — stored + forwarded, never
  parsed (invariant #2). All three clients mint the genesis self-sig on a
  new-key publish and verify a served genesis self-sig against the served key on
  a first observation. A leaked append secret can no longer fabricate a keyless
  leaf. **Residual (at the time of ADR-0019): genesis is TOFU** — a self-sig does
  not PREVENT first-entry squatting (an attacker with the append secret can
  self-sign their OWN key for a victim's never-used account_key); it is detected
  via CONIKS self-monitoring + the ADR-0016 fail-closed peer gate. The
  auth-attested-genesis hardening (which PREVENTS squatting) was recorded as the
  elective alternative in ADR-0019 — **now implemented by ADR-0036, see the update
  below.**

- Finding 3 (mismatch / absent posture) is UNCHANGED — still surface-not-block
  (a chain contradiction maps to `mismatch`, not a block; only `rollback`
  hard-fails), consistent with the deferred `lakko-security` ruling.

Two new attacker-touchable verify surfaces got fuzz targets:
`verify_supersession_chain` (rotation chain) and `verify_leaf_genesis` (genesis
self-sig). `lakko-security` re-reviewed Sprints 1+2: **APPROVE-WITH-NOTES**
(invariants #2/#4/#6 hold or strengthened; cleared to merge). See ADR-0019.

**Update — G-30 #1 closed by ADR-0020 (witness co-signing), 2026-06-13.**
Finding 1 (operator split-view / equivocation across DIFFERENT signed roots) is
CLOSED for the single-witness model:

- **An independent witness co-signs every operator root** — but ONLY after
  verifying a Merkle **consistency proof** (RFC 6962 §2.1.2,
  `crates/lakko-transparency/src/merkle.rs`) that the new root append-only-extends
  the witness's last-co-signed root. Two divergent forks cannot both extend one
  root, so a malicious operator cannot obtain a co-signature on a fork: the honest
  witness (`services/witness`, `POST /witness/cosign`) refuses (409 `NotConsistent`)
  and advances its `(root, log_size)` high-water under a lock.
- **Clients require operator AND witness.** All three pin the witness key
  build-time and run `witnessGateRefuses` immediately after the no-rollback gate
  in `verifyIdentityInLog` + `verifyRotationChain`: a root with no cosig OR an
  invalid one (`verify_witness_cosig` over the served `(tree_root, log_size)`,
  domain `lakko/transparency/witness/v1`) is **refused** (`mismatch`, fail-closed,
  invariant #4). No pinned key → operator-only transition posture. The FFI is
  verify-only; signing stays server-side.
- **The operator serves co-signs-on-read, fail-closed.** `services/transparency`
  signs the head root fresh per `/root`, reuses the cached cosig keyed on
  `(tree_root, log_size)`, re-obtains it only on growth, and serves **None**
  (never a stale cosig) when the witness is unreachable — a stale cosig can never
  bind-mismatch a client.
- Two new fuzz targets: `verify_consistency` (the RFC 6962 verifier, a fork must
  fail) + `verify_witness_cosig` (the 105-byte cosig blob parse + bind + verify).
- **Residual:** operator+witness COLLUSION — the single-witness case. **The
  K-of-N witness quorum that raises this bar is now implemented
  ([ADR-0039](../adr/0039-kt-log-witness-quorum.md), 2026-07-21):** the root gate
  now needs the operator signature AND ≥ K valid co-signatures from DISTINCT keys
  in a build-time-pinned witness key set of size N
  (`crates/lakko-transparency/src/witness.rs` `verify_witness_quorum`,
  dedup-by-distinct-key so a replayed / duplicated cosig can never inflate the
  quorum; the transparency service fans out to N witnesses best-effort and serves
  `witness_cosigs_b64: Vec<String>`, the clients enforce the threshold
  fail-closed; N=1/K=1 is the single-witness gate). So a fork now requires
  corrupting K witnesses, not one. The residual is now operator + K COLLUDING
  witnesses, closed in practice ONLY under INDEPENDENT witness operation (the N
  witnesses in distinct trust domains; a deploy/ops property, not code-enforced —
  a single operator running all N witnesses gains nothing). The deferred
  client-gossip layer would remove the witness-collusion residual entirely.

`lakko-security` reviewed the full arc (Sprints 1-6): **APPROVE-WITH-NOTES**
(invariants #2/#4/#6 hold or strengthened; one trivial hygiene finding —
`#![forbid(unsafe_code)]` on the witness service — applied). See ADR-0020.

**Update — G-30 finding 2: genesis squatting PREVENTED (not just detected) by
ADR-0036 (auth-attested genesis), 2026-07-20.** The ADR-0019 genesis self-sig
above only DETECTS first-entry squatting; ADR-0036 adds an **auth-attested
genesis** proof that PREVENTS it, additive to (never replacing) the self-sig.

- **The attestation.** A 153-byte fixed-layout Ed25519 blob
  (`crates/lakko-attest/src/genesis.rs`, domain `lakko/genesis-attest/v1`,
  distinct from device-attest) binding `(account_id, username_hash, pubkey,
  expiry)`. Auth mints it at `POST /auth/genesis/attest` — bearer-authed only, so
  holding a valid session token (proof of OPAQUE authentication) is the entire
  basis; **only the account holder can mint a genesis attestation for their own
  account.** A squatter cannot OPAQUE-authenticate as the victim's `account_id`,
  so cannot obtain auth's signature over the victim's fields. Stated plainly: this
  is a binding of the OPAQUE-authenticated account to a client-ASSERTED key, not
  an independent proof the key is "right" (auth has no such ground truth under
  invariant #2) — which is exactly sufficient, because squatting is precisely the
  case the binding rules out.
- **The bridge derivation.** Transparency never learns `account_id`, so the
  attestation carries `username_hash = BLAKE3("lakko/transparency/account/v1" ‖
  account_id)`, promoted to the one shared `lakko_transparency::account_username_hash`
  + a golden drift test (a one-byte domain drift would silently fail-close the
  gate).
- **The append gate (defence-in-depth, fail-closed).** The genesis/reset arm of
  `services/transparency/src/store.rs` runs `verify_genesis_fresh` (bind
  `username_hash`+`pubkey` + freshness) behind default-OFF
  `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` + pinned
  `LAKKO_TRANSPARENCY_GENESIS_ATTEST_KEY`; under the flag an absent, unpinned-key,
  or invalid/expired attestation refuses the append (invariant #4). Discovery
  threads `genesis_attest` opaquely (migration `0016`, `COALESCE`-preserved,
  invariant #2). All three clients mint on the genesis publish (soft-fail) and
  verify a served attestation on first observation beside the self-sig
  (`verify_genesis_binding`, no freshness — a permanent artifact — fail-closed
  `mismatch`), reusing the pinned DeviceAttestKey. New fuzz target:
  `genesis_attestation_open`.
- **Residual.** Now the DEPLOY ROLLOUT — flip the flag to on once all three
  clients mint (S6, done) — plus a live cross-service end-to-end (mint → publish →
  append-gate → first-observation verify) on a deployed environment. Operator+auth
  collusion stays out of scope (a compromised attest signing key already co-signs
  the root — the same trust tier). See ADR-0036.

## Client release / update

### G-32: Update-notice suppress/replay residual (ADR-0038) — **ACCEPTED (LOW), 2026-07**

ADR-0038 adds a signed "update available" notice: the client fetches a **global**,
Ed25519-signed `ReleaseManifest` (89 bytes) as a raw static blob from
`/.well-known/lakko-release`, verifies it against a build-time-pinned **dedicated
publisher** key, and — if its build-time `CURRENT_RELEASE` is behind — shows a
dismissible banner (mobile) with a store deep-link. The signature is an
**anti-forgery** control: a malicious serving tier / MITM **cannot** forge a higher
`latest_release` or `min_supported_release` (a phishing nudge toward a hostile store
link, or manufactured urgency) without the offline release signing key.

**Residual (accepted, LOW).** The same adversary CAN **suppress** the notice (strip
the file → 404 → the client renders no banner) or **replay** a genuine-but-old
manifest (→ an older asserted `latest`). Both are LOW: they keep the user on a
version they **already run** — no confidentiality or integrity break, and the store
link the button opens is unaffected. The client's own fail action on an unverifiable
manifest is *also* "show no banner" (fail-toward-silence), so a suppressor gains
nothing a stripped file would not already give them. Freshness is **DISABLED in v1**
(`max_staleness = i64::MAX`), so a replayed old manifest is not rejected as stale — a
deliberate call given the fail-action is silence and the re-sign-cadence operational
coupling (a static manifest whose `signed_at` ages would otherwise make the check go
dormant everywhere unless the publisher re-signs on a cron).

This is an **availability / anti-phishing** posture, **not** an E2EE control: its
"fail-safe" means *fail toward no nag*, distinct from invariant #4's
fail-closed-refuse-the-connection — it touches no message-path or key-material
invariant. Invariant #1 holds trivially (the manifest is global — no `account_id`, no
per-user field); invariant #2 holds (a public Ed25519-signed static artifact nginx
serves opaquely, no key material). Because it adds no E2EE / MLS / call invariant, it
takes **no indexed C-number**, so the invariant index is unchanged by it (it stood at
8 I + 58 C + 15 T when this was written; the live counts are in the gated Quick stats
table in [`invariant-index.md`](./invariant-index.md), which is the only place they
are maintained). A future tightening (a persisted highest-`latest_release`-seen floor, or a re-enabled
generous freshness window) is a clean addition if the residual is ever judged worth
closing. See ADR-0038 § "Freshness model + residual".

## AEAD (key, nonce)-reuse audit

### G-33: Channel publish-counter not persisted → AEAD nonce reuse across relaunch — **CLOSED (2026-07)**

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

### G-34: System-wide AEAD (key, nonce)-reuse audit — **T1 CLOSED + S1/S2 HARDENED + T2 (chat-ratchet nonce reuse) CLOSED (2026-07)**

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

## Push routing

### G-36: Push routing under an opaque `push_gid` — the co-membership cluster and the surviving delivery-side residuals ([ADR-0042](../adr/0042-push-opaque-group-routing-id.md) + [ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md), 2026-08)

[ADR-0042](../adr/0042-push-opaque-group-routing-id.md) landed end-to-end (core +
FFI + web + iOS + Android; `lakko-security` **APPROVE-WITH-NOTES**, no
CRITICAL/HIGH) and is recorded as **C-60**. This entry is the honest inventory of
what it did **not** close. Like G-33 / G-34 / G-35 this is a **free-form audit
record** — it adds **no** indexed C / I / T invariant of its own (C-60 is ADR-0042's
positive property and **C-61** is ADR-0043's; the counts moved to
**8 I + 61 C + 15 T** because of those two, not because of this gap, and have moved
again since — the live figures are in the gated Quick stats table in
[`invariant-index.md`](./invariant-index.md)).

**Update (2026-08-11) — [ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md)
landed on all three clients (`935250a` web, `a6ed544` iOS + Android), rotating the
secret on member removal.** It **closes residual 2** (both halves), **widens
residual 3**, leaves **residual 1** substantively intact, and **adds residuals 5
and 6**. C-60 is **refined** rather than renumbered (the routing id is the same
property, now also forward-unlinkable across a removal), and the sequencing rule
the rotation depends on is pinned as the new **C-61**. The `lakko-security` pass
ADR-0043 declares mandatory for option (b) **was run: APPROVE-WITH-NOTES, no
CRITICAL/HIGH, no invariant #2/#3/#4 break on the live path**, with one blocking
MEDIUM (**F1** — a coalescing rotation lock on web + iOS folded a second, distinct
removal into an in-flight rotation, handing the second evictee the new secret;
Android's serializing `Mutex` was already correct) now fixed on both platforms by
adopting Android's lock. Its remaining findings are carried as residuals 3, 5, 6
and 7 below.

**What ADR-0042 actually bought, stated precisely.** Push subscribe / notify / the
delivered payload now route under `push_gid = keyed-BLAKE3(push_routing_secret,
"lakko/push/group-routing-id/v1")` over 32 CSPRNG bytes that never leave the E2EE
group, so the routing id is **algebraically independent of the DS `group_id`** and a
push-gw↔DS cross-correlation is impossible without the secret. That removes the
**linkage to the DS group identity**. It does **not** remove the **clustering**.

#### Residual 1 — the co-membership cluster is still materialised (the main one)

`services/push-gw` still stores device↔routing-id rows
(`push_group_subscriptions`), and APNs / FCM still receive a **stable per-group
pseudonym in cleartext** (the wake payload `g` plus the APNs `thread-id`, both
`hex::encode` of the routing id — `providers/apns.rs:368`/`:371`,
`providers/fcm.rs:369`). A provider (or push-gw itself) can observe "these N device
tokens share one opaque group handle" and track that cluster **and its churn** —
it simply can no longer join that handle to the DS journal key. The set-membership
and its evolution over time remain visible; only the group's *name* is opaque.
WebPush is unaffected either way (the whole `WakeBody` is RFC-8291
`aes128gcm`-encrypted before it leaves push-gw, `providers/webpush.rs:252-256`).

**ADR-0043's rotation does NOT close this, and the precise reason matters.**
Rotation **renames** the cluster; it does not unlink it. A rotation is not a
mixnet — the device-token set is identical-or-near-identical either side of the
boundary, so the provider **re-links across it by token-set intersection**. Worse,
the set *difference* across the boundary **names the evicted device**, which is
arguably a signal the stable id did not hand over so legibly: before ADR-0043 a
removal was invisible to the provider (nothing changed); after it, a removal is a
visible re-keying event with an identifiable diff. That trade was made
deliberately — it buys the closure of residual 2, a live channel to an
already-adversarial evicted party, at the cost of legibility to a party that
already watches the cluster continuously — but it is a judgement, and it means the
cluster leak is **unchanged in substance**.

Closing this needs **per-device-blinded routing ids** — each subscriber
registering under an id only it and the publisher can compute, so no two devices
ever share a key-space value — plus payload minimisation. That is the first change
in this arc that would **not** leave `services/push-gw` untouched (notify would
need per-recipient ids rather than one JOIN key). A rotating id is not a
substitute.

#### ~~Residual 2 — no rotation on membership change, compounded by dead unsubscribe~~ CLOSED (2026-08-11, ADR-0043)

**The original finding.** V1 deliberately did not re-mint the secret when a member
was removed, so **a removed member kept a valid `push_gid`** and kept deriving the
group's current routing id — an ongoing post-eviction metadata channel ("this group
was active at time T"), not merely stale rows. This was compounded by a second,
independent finding: **per-group unsubscribe was never called anywhere.**
`unsubscribeGroup` existed on all three clients
(`apps/web/src/push/client.ts:204`, `apps/ios/Lakko/Push/PushClient.swift:165`,
`apps/android/.../push/PushClient.kt:197`) and the `POST /push/group/unsubscribe`
endpoint was live (`services/push-gw/src/main.rs:250`), but **no call site existed**
— dead code on every client. The two compounded: rotation alone would not help
while the stale subscription persisted, and unsubscribe alone would not help while
the ex-member could still derive the id.

**Both halves are now closed by [ADR-0043](../adr/0043-push-routing-secret-rotation-on-removal.md).**

- **The wake stream ends — but state the mechanism exactly.** On a membership
  shrink the owner **or an admin** (ratified option (b)) re-mints and
  redistributes the secret, sealed at the **post-remove epoch**, so RFC 9420
  forward secrecy denies the evicted leaf the envelope. The ex-member's push-gw
  row is **not deleted by anyone else** — nobody else *can* delete it
  (`group_unsubscribe` removes only the `(device, group)` row named in the request,
  and a device knows only its own pseudonym) — so from the group's side it goes
  **INERT**: nothing ever notifies the old gid again. **That inertness is the
  win** — the ex-member stops learning the group is active — and it is
  deliberately *not* cleanup.

  > **AMENDED 2026-08-29 (`8763f3e`): the clause "and no client has 'you were
  > removed' handling" no longer holds, and with it the row's permanence.** The
  > ADR-0045 self-eviction work built exactly that handling — `Group::is_active()`
  > surfaced through both FFI surfaces, and a `retire-push-subscription` teardown
  > step on all three clients — so **the evicted device deletes its own row**
  > (`ENDED_TEARDOWN_STEPS` in `apps/web/src/chat/membership-ended.ts` and its two
  > native twins, reusing the same `retireOldPushGid` the rotation teardown uses).
  > That closes the half of this residual ADR-0043 recorded as unclosable. **The
  > bound is real and is stated rather than glossed:** it needs the evicted device
  > to come online and run it, so it **complements** rotation and replaces nothing —
  > a device that never returns still leaves its row, inert.
- **Per-group unsubscribe stops being dead code.** Its **first real caller** on
  every client is the receive-side teardown, in which each remaining member
  retires **its own** old gid (unsubscribe + delete the reverse-index row): web
  `apps/web/src/push/group-routing.ts:336`, iOS
  `apps/ios/Lakko/Push/PushGroupRouting.swift:220`, Android
  `apps/android/.../chat/ChatRepository.kt:1670`. This half is **required, not
  hygiene**: without it every member would leave one live push-gw row per
  rotation, turning a single live cluster into a live cluster **plus a frozen
  archive of every previous one** — a strictly richer artifact for residual 1 than
  not rotating at all.

Rotation fires only on a membership **shrink**, detected as a set difference over
member Ed25519 signature keys (not counts, not leaf indices — OpenMLS reuses a
vacated leaf for the next Add). It **never mints**: a device holding no secret is a
no-op, so widening rotation to admins cannot create a second minter.

#### Residual 3 — admin denial of service, WIDENED from seed to churn (ADR-0043)

The `62f23ad` refinement widened the receive gate from owner-only to **owner +
admins** to fix a real delivery regression (ADR-0022 lets an admin add members, and
a member added by a non-owner admin would otherwise never receive the secret). At
that point an admin could **seed** a secret into a hole but never **overwrite** a
stored one, so the residual was: a malicious admin can seed a bogus secret to a
member who holds none, denying that member's wakes until the owner next rotates.

**ADR-0043's ratified option (b) widens this.** The gate's admin row flipped from a
refusal to an acceptance (`reject-admin-overwrite` -> `accept-admin-rotate` on web;
the natives keep a binary accept/reject and simply flip the row), so an admin may
now **overwrite whatever is stored**. The residual therefore generalises from *"a
malicious admin can seed a member holding none"* to **"a malicious admin can churn
the whole group's routing id at will"**, repeatedly denying **every** member's
wakes until the owner rotates back.

Still bounded, and for the same reasons as before: foreground polling keeps
delivering messages, so this is a wake-latency denial rather than message loss; and
an admin can already remove members outright, so the capability gain is small. But
the widening is real and is recorded rather than waived — it re-opens the shape of
the `lakko-security` **MEDIUM-1** fix (`21554f2`), which existed precisely to stop
a non-owner from unilaterally rotating the routing id.

**Android additionally deleted a prior hardening to make (b) work.** The
strip-admins-on-unreadable-store rule (from the MEDIUM-1 arc) narrowed the admin
set when the secret-store read *threw*, so an unreadable store could not be
mistaken for a fillable hole. Under (b) that premise is gone — an admin may
overwrite whatever is stored — so keeping the rule would have refused the very
admin rotation (b) exists to permit, on nothing more than a transient store error.
It is removed (`GroupPushRouting.kt:499-504`, historical note `:464-474`). Who may
write is still decided by the role table and is still fail-closed; an unreadable
**role table** (as opposed to an unreadable secret store) still refuses. Web and
iOS encoded the same premise inline via a `storedUnreadable` flag rather than by
rewriting the authority, and both kept the parameter in a demoted role (web picks
*which* accept to report; iOS keeps it as an unread diagnostic).

**What did NOT change, and is what keeps (b) bounded:** rotation **never mints**.
Option (b) widened *overwrite* authority only, so an admin can replace a secret the
group already holds but can never bring one into existence — minting stays
owner-only, so the group's `push_gid` can never diverge into two.

**One further consequence, and the server-side premises it rests on.** Under (b) a
malicious admin can keep **re-establishing** a routing id it shares with an evicted
party *after each owner rotation*, rather than only leaking the current one once.
This is **not a new capability class** — an admin could always exfiltrate the
current secret out-of-band — but it turns a one-shot leak into a **sustained** one,
because the admin can re-converge the group onto a secret of its choosing every
time the owner rotates away from it. Two server-side facts make that directly
exploitable and are worth stating as premises rather than leaving implicit:

- **`POST /push/group/subscribe` performs no membership check.** push-gw cannot
  perform one — it is opaque to group membership by design (invariant #2) — so a
  shared secret is *directly subscribable* by a non-member. Possession of the
  secret **is** the authorisation.
- **`POST /push/group/unsubscribe` carries no ownership proof.** It deletes
  whatever `(device, group)` pair is named in the request and is PoW-gated only.
  So the claim elsewhere in this entry that "no remaining member can tear down the
  ex-member's row" rests on **the device pseudonym never being published**, not on
  any server-side authorization. That is a confidentiality assumption about an
  identifier, not an access-control guarantee.

Accepted, not closed. This residual is the substance of the `lakko-security` pass
ADR-0043 declares mandatory for option (b), which **was run — APPROVE-WITH-NOTES,
no CRITICAL/HIGH** — and which recorded both server-side premises above as explicit
judgements rather than leaving them implicit.

#### Residual 4 — legacy / no-role-table groups get no push at all

The send side mints only when the local member is the ADR-0022 **owner**. A legacy
or open group has **no role table**, hence no owner, so **nobody mints** (the
in-code rationale is explicit: "nobody mints (avoids divergence)",
`apps/web/src/push/group-routing.ts`). Every member therefore **defers** — a member
without the secret does not subscribe at all rather than falling back to the leaky
raw `group_id`. The privacy posture is the preferred one (invariant #4 — no silent
downgrade), but the functional consequence is real and silent: **pre-ADR-0022 groups
receive no push wakes whatsoever**, with no user-visible signal. A provisioning path
for legacy groups (or an explicit UI state) is unbuilt.

**ADR-0043 does not change this, and adds a second clause: such groups now also
never rotate.** The rotation decision refuses a group whose local role resolves to
neither owner nor admin, which a no-role-table group always does
(`skip-not-authorized`). Note the asymmetry, because it is easy to invert: the
**receive gate** *accepts* a changed secret in a no-role-table group
(`accept-no-owner` on web; the same branch on both natives) — that is the legacy /
open-group behaviour ADR-0042 established. It is the **rotation** decision that
refuses. Receive-accept and rotate-refuse are different questions about the same
group.

#### Residual 5 — the old gid can be orphaned, and NOT only by a crash (new, ADR-0043) — **one of its three paths (F-E, rotate-vs-receive) is CLOSED**; the crash and store-read-failure paths remain accepted

ADR-0043 prescribed a durable pending-rotation record modelled on the ADR-0040
commit outbox. **It was deliberately not built**, because its stated justification
went stale between that ADR being written (`5665018`) and being implemented: the
three commits `5729aa0` -> `9cb8f06` -> `7e7d143` had meanwhile added a durable
`distributed` flag on the secret row plus a re-broadcast branch open to **any**
holder, reached from both chat-open and the launch sweep — so a failed rotation
broadcast is already retried without a new record.

**What the flag does not cover** is the other side effect. It recovers the
**broadcast**, never the **teardown**: nothing anywhere records the OLD gid
awaiting retirement, which lives only as an in-memory local between the persist and
the unsubscribe. Once the secrets row is overwritten (it is keyed
`(account, group)` and clobbered in place) the old bytes are unrecoverable, the old
`push_gid` can never be named again, and its push-gw subscription plus
reverse-index row become a **permanent orphan**.

**Three paths reached it, and neither of the two that survive involves a crash at
all** (the third is now closed):

1. **A crash** between the persist and the teardown.
2. **A transient secret-store read failure on the receive path.** The teardown is
   conditioned on the *previously stored* secret, snapshotted before the persist.
   If that read fails it yields `stored = null` on all three clients — which is
   indistinguishable from "this device held nothing" — so the new secret is
   persisted and the teardown is simply **skipped**. No crash, no error surfaced,
   same inert orphan. This is **more likely** than the crash window, since it needs
   only a momentary store hiccup rather than process death inside a narrow window.
3. **~~A rotation/receive interleaving~~ (`lakko-security` **F-E**) — CLOSED**
   (`9a0f539`). Rotation and receive were **not mutually excluded** — no client's
   receive gate took the rotation lock — so a peer's envelope could interleave with
   a local rotation and leave an **intermediate** gid subscribed with nothing
   pointing at it. It was bounded the same way as the other two (every candidate id
   in that interleaving is **post-removal**, so never an id an evicted member can
   derive), and it is now closed outright because the residual-10 fix put receive,
   rotate, ensure and redistribute on **one shared** per-`(account, group)` lock —
   they exclude each other rather than each merely excluding itself. Android
   **proved the sharing is what does it**, by probing with receive on its own
   separate lock: the F-A and N-rotation cases passed while **F-E still failed**.
   On iOS this was the interleave that was genuinely real (its rotation parks in
   its broadcast), and its test failed four assertions before the fix.

**Verdict: accepted, and the durable record should NOT be built** — an explicit
`lakko-security` judgement on this arc, not merely this entry's opinion. The orphan is
**inert** — nothing ever notifies the old gid again — so this is an accumulation
defect, not a live channel, the same class as residual 2's ex-member row. What the
row leaks is bounded to "this device was once in *some* group", with no linkage to
which group or to the current cluster. Building the durable record to reclaim it
would cost a persist on a hot path plus a launch sweep, which is not proportionate
to reclaiming a row that leaks nothing further. Recorded rather than fixed. (Were
it ever built, it would be **lighter** than ADR-0043 prescribed: the new secret is
already durable by then, so only the *old gid* needs recording — an "old gids
awaiting retirement" list.) All three paths are accepted on the same reasoning.

#### Residual 6 — the post-Remove-epoch ordering (C-61) was enforced structurally, with no test — **CLOSED** (2026-08-14, [ADR-0044](../adr/0044-push-routing-secret-recency.md))

The whole of residual 2's closure depends on the rotation broadcast being emitted
**strictly after** the Remove commit, so that the envelope is sealed at the
post-remove epoch and RFC 9420 forward secrecy denies it to the evicted leaf. That
rule is pinned as **C-61**. As ADR-0043 shipped it was enforced **structurally
only** — by call-site sequencing (an awaited publish, or a lock block that closes)
on each client — with **no test on any client**; only the sibling
read-old-before-overwrite rule was test-pinned. ADR-0043 itself states the
ordering "must be asserted in a test"; that requirement was unmet.
(`lakko-security` **F3**, LOW — the review reached the same finding
independently.)

**The dependency sharpened before it was closed, which is why the test stopped
being optional.** Under ADR-0043 alone, a C-61 regression cost the rotation's
**forward secrecy**: the envelope would be sealed at the pre-removal epoch and the
evicted member could open it. Under **ADR-0044** it costs that **and inverts the
recency comparison** — a rotation stamped at the pre-removal epoch `N` **ties**
with the stale `S_old` minted at `N`, so the tie falls to the 32 secret bytes and
the rotation may simply **lose to the stale secret**. The guarantee degrades from
"the rotation always wins" to **a coin flip**, silently. ADR-0044 therefore
converts C-61 from a property protecting one guarantee into a **precondition of
two**, and landed the regression tests in the same arc rather than deferring them.

**CLOSED by `014e84e`** (web's landed in `936d849`). What that means concretely:

- **One named seam per client**, so the two steps cannot be reordered
  independently — e.g. iOS
  `PushGroupRouting.publishRemoveThenRotate(publishRemove:rotate:onRotationStarted:)`,
  whose sole production call site is `ChatRepository.removeMemberFromGroup`.
- **The assertion is on the emitted envelope's `mint_epoch` stamp, not on call
  order** — a strictly stronger check, since it fails on any path that reaches the
  broadcast with a pre-removal epoch, however the reordering happened.
- **A permanent inverted-order companion on each client** that runs the swap and
  asserts the consequence: web `the INVERTED order would stamp the pre-removal
  epoch and lose the guarantee`; iOS
  `testTheInvertedOrderStampsThePreRemovalEpochAndTheRotationLoses`; Android
  `` `C-61 - the INVERTED order stamps PRE-removal and the rotation LOSES` ``
  (which additionally asserts the *gate* agrees, resolving the stale replay as
  `ACCEPT_OWNER` over the rotation — F-A reproduced under the swap).
- **On both natives the discrimination was proven by running the swap, not
  asserted**: iOS temporarily added an inverted copy and recorded three failures;
  Android replaced the seam with rotate-then-advance and recorded the stamp
  assertion failing 11 vs 10. Both probes were reverted.
- Counts: **six** C-61 tests on web, **five** on iOS, **six** on Android.

**The fixture is load-bearing, and its first cut was latently flaky.** A test that
exists to prove the rotation *loses* under the swap must make it lose with
certainty, and at an epoch tie the winner is the bytewise-greater secret. The web
fixture landed in `936d849` filled the stale secret with `0xEE`, so a freshly
minted **random** rotation secret sorts higher roughly once in fourteen runs and
the rotation would have **won** — passing a test written to watch it lose. All
three now use `0xFF`, the bytewise maximum, so the rotation loses
deterministically. Recorded because the failure mode generalises: a
discrimination test whose fixture does not force the outcome is a test that
sometimes proves nothing, silently.

#### Residual 7 — concurrent rotators can strand a device on a superseded secret (ADR-0043; `lakko-security` F2, MEDIUM) — **CONCURRENT-ROTATOR CORE CLOSED for the single-context case** ([ADR-0044](../adr/0044-push-routing-secret-recency.md) + the serializing lock, `9a0f539`); the F-D stale-role window and the no-repair limit remain open

A rotator applies **its own** mint at **local** time, not at its own position in
the DS journal. So a device that publishes its rotation at journal id `p` and then
polls an earlier message `q < p` it had not yet seen will **overwrite its own newer
secret with the older one** — and the row is then marked `distributed: true`, so
nothing ever re-broadcasts to repair it.

Under option (b) this is **routine rather than a race**: one removal produces
**N+1 rotations** in a group with an owner and N admins, because the committer
rotates from its own remove path while every other owner/admin observes the same
shrink through its poll diff and rotates too. The affected device keeps deferring
or routes under a gid nobody notifies, self-healing only at the next rotation.
Foreground polling delivers messages throughout, so this is a **wake-delivery
defect, not message loss**.

**What ADR-0044 built, and what it took to make it hold.** The root cause above is
*mutual incomparability*: the N+1 authorised mints had no order between them, so
which one a device ended on was decided by arrival order and devices
**partitioned**. ADR-0044's total order on `(mint_epoch, secret_bytes)` removes
exactly that — the N+1 mints from one removal share an epoch (no intervening
commit), so the 32-byte tiebreak decides deterministically, identically for every
participant, and independently of arrival order. **This is the load-bearing reason
the tiebreak is not a corner case:** epoch alone would leave the N+1 mints
incomparable.

**The order alone was not enough, and that is the arc's lesson.** The N+1
same-epoch rotations typically arrive in **one poll batch**, and as first shipped
the receive gate was an unserialized read-modify-write dispatched fire-and-forget
per envelope — so all N+1 gates compared against a **single pre-batch snapshot**
and the survivor was decided by **scheduling order**. Stated exactly: **the total
order's arrival-order independence holds for the pure function, but not
automatically for the path that calls it.** It becomes a property of the device's
resulting state only once the gate consuming it is serialized. That serialization
landed in `9a0f539` — one per-`(account, group)` lock spanning the whole
read-decide-write, shared across receive / rotate / ensure / redistribute — and is
now pinned as **C-62 sub-invariant 5**.

**CLOSED, with the qualification stated precisely.** The concurrent-rotator core
is closed **for the single-context case**: within one JS realm (web), one app
process (natives), and — on web — across tabs where `navigator.locks` is
available. The remaining un-serialized boundaries are the re-scoped **residual
10** and are the qualification on this closure, not a separate reservation.

**Three further things survive the lock and remain open.**

1. **The F-D stale-role window, untouched and orthogonal** (below). A demoted
   rotator's envelope is refused on **authority**, not on recency, so an ordering
   predicate has no bearing on it whatsoever.
2. **An ordering gate stops a stale value SPREADING; it does not repair an
   already-stale holder.** The order pushes nothing. A device that ends on a loser
   converges only when an authorised newer envelope actually reaches it — in the
   normal case the winner still sitting in the journal ahead of its cursor, but
   **not** if it has already polled past that position (or the 30-day journal
   prune has taken it). A self-healing reply-with-my-newer-one was considered and
   **deferred**: it needs a storm guard, since N members each answering one stale
   broadcast is an N-fold journal amplification.
3. **The N+1 rotations themselves still happen.** The order governs the *final
   state*; it does not reduce the number of mints or the transient subscribe /
   unsubscribe churn they cause. Their **F-E orphan-gid exposure is now closed**
   by the shared lock (residual 5, path 3), but residual 5's crash and
   store-read-failure paths still apply to each of the N+1.

**The web-only stale-role sub-case remains fixed** — that is true for the case it
names (the `ownRole()` signal holding a pre-poll value; web now reads the role off
the live group via `readOwnRoleNow`).

**A narrower stale-role window survives on ALL THREE, and is narrowed rather than
eliminated** (`lakko-security` **F-D**). Every client captures the role **before**
the rotation queues behind a predecessor on the serializing lock — web
`Chat.tsx:2297` (`await readOwnRoleNow()` feeding a call that then takes the
`rotating` chain), iOS `ChatRepository.swift:1590` (`ownRole(groupIdHex:)` read
before the `Task` at `:1591` chains), Android `:1960` and `:1997`
(`rotationRole(...)` read before `pushScope.launch`). A **demotion that lands during
the queue wait** therefore rotates on a **stale** role: peers run the receive gate
against the *current* role table, refuse the envelope, and the rotator **strands
itself** on a secret nobody else accepts. **Availability-only** — it is the
fail-closed gate behaving correctly — and it **self-heals at the next authorised
envelope**, since the rotator's row still accepts any owner/admin write. Not fixed;
closing it would mean re-reading the role *after* acquiring the lock.

Note this also falsifies the convergence argument ADR-0043 originally offered for
option (b) ("the DS journal is a total order, so last-accepted-wins converges"),
which is corrected in that ADR.

#### Residual 8 — the receive-side teardown can precede the new subscription (new, web only; `lakko-security` F4, LOW)

On web the receive-side teardown may run **before** the new subscription is
established, leaving the device briefly subscribed to **neither** the old gid nor
the new one. A wake arriving in that window is not delivered; the message itself is
unaffected and arrives on the next poll. Recorded as a follow-up, not fixed.

#### Residual 9 — a stale `pending` re-broadcast can silently UNDO a rotation, permanently (ADR-0043 re-review; `lakko-security` F-A) — **CLOSED for the single-context case** ([ADR-0044](../adr/0044-push-routing-secret-recency.md) recency order + the serializing lock, `9a0f539`)

The one residual on this list whose consequence is **confidentiality rather than
wake delivery**, and it needs **no malice and no crash** — only a failed broadcast
followed by someone else's rotation. The mechanics below are kept as the
historical record; the status of the attempted fix is at the end of this entry.

1. An owner/admin's post-add `redistributeSecretToGroup` **broadcast fails**. The
   failure path marks the row `pending` (`markGroupRoutingPending`), so that device
   now holds the **PRE-rotation** secret `S_old` in an un-settled row.
2. Meanwhile another admin removes a member and rotates the group to `S1`
   (ADR-0043, ratified option (b) — any owner *or* admin may rotate).
3. The first device runs `ensureGroupPushRouting` **BEFORE** it applies the `S1`
   envelope. Its `pending` branch — the any-holder re-broadcast added in `7e7d143`
   — re-sends **`S_old`**.
4. Every member resolves that envelope as `accept-owner` / `accept-admin-rotate`,
   persists `S_old`, **and retires `gid(S1)`**. The group migrates **BACK** onto the
   id the evicted member still derives and is still subscribed to.
5. No further membership shrink occurs, so **no further rotation fires**. The state
   is **permanent, not transient** — unlike residual 7, nothing self-heals it.

**Root cause, stated plainly: the receive gate has no recency or monotonicity
notion at all.** `routingSecretVerdict` (web) and its iOS/Android twins ask only
*who sent it* — owner / admin / member, against the ADR-0022 role table — and never
*is it newer than what I hold*. Every input to the verdict is an authority question.
The any-holder re-broadcast branch and rotation are individually sound; they
interact badly precisely because no ordering predicate exists to separate a
**retry** of the current secret from a **replay** of a superseded one. The two are
byte-indistinguishable to the gate.

**Reachability differs per client**, and the ordering that narrows it is incidental
rather than designed:

- **Web** narrows it: `Chat.tsx:2497` `await refresh()` before `provisionPushRouting()`
  at `:2504`, so a normal chat-open applies the pending `S1` envelope first. But
  `refresh()` catches its own errors (`Chat.tsx:1694-1698` — `setError` and return,
  never rethrow), so a transient poll failure resolves the await normally and lets
  provisioning run anyway. Narrowed, not closed.
- **iOS** (`ChatRepository.swift:1471`) and **Android** (`:229`, `:339`, `:375`,
  `:1888`) fire provisioning on **independent tasks** with no such ordering, so the
  window is **wider** there.

**Severity: recorded, not blocking.** The worst case is **exactly the pre-ADR-0043
status quo** — the evicted member keeps receiving wakes under an id it already
holds — plus some inert orphan rows of the residual-5 class. It fails **TO** the old
behaviour, never below it: no member is downgraded to the leaky raw `group_id`
(invariant #4 holds), the DS and push-gw stay opaque (invariant #2), and no message
content is exposed. What it does is silently **revoke the ADR-0043 win** for that
group, with no user-visible signal — which is why it is recorded here rather than
folded into residual 7.

Note also that **residual 7's framing understates this variant.** That entry closes
with "a wake-delivery defect, not message loss", which is accurate for its own
scenario (a device stranded on a superseded secret misses wakes). This one is not a
delivery defect at all: its consequence is the **confidentiality** property C-60
sub-invariant 6 and C-61 exist to protect.

**Fix direction — recorded as a follow-up, deliberately NOT built in ADR-0043:**

1. **The general fix** — carry a **monotone counter or the minting epoch inside the
   62-byte distribution envelope** and refuse a non-increasing value on receive.
   This gives the gate the recency predicate it structurally lacks and closes the
   whole class, not just this path. It is a **`crates/lakko-contacts` wire change**
   (the envelope is `domain(29) ‖ version(1) ‖ secret(32)`, a fixed-length payload
   with a version discriminator), so it needs an envelope version bump, the
   fail-closed codec extended, its fuzz target updated
   (`push_routing_secret_decode`), and all three clients moved together.
2. **The narrow fix** — clear or skip the `pending` re-broadcast once a
   **different** secret has been accepted. Purely client-side, no wire change, but
   it patches this path rather than the missing predicate.

**CLOSED, in two steps, and the second step is the interesting one.**
[ADR-0044](../adr/0044-push-routing-secret-recency.md) built fix direction 1, the
general one (`936d849` core + FFI + web, `014e84e` iOS + Android). The
`lakko-security` review of that arc nonetheless returned **REQUEST CHANGES** and
**reproduced this residual by execution** on web, driving two envelopes
concurrently the way `Chat.tsx` actually drives them and observing verdicts
`['accept-admin-rotate', 'accept-identical']` with a final stored secret of `0xEE`
**at epoch 10** — the stale one. The reason was **residual 10**, not a fault in the
primitive: the ordering gate was correct and armed, but the receive path was an
unserialized read-modify-write dispatched fire-and-forget per envelope, so the gate
was handed a **stale snapshot** and was **bypassed, not defeated**. The vehicle was
`accept-identical`, which deliberately short-circuits **both** the future clamp and
the total order — correct for a benign retry, and ADR-0044 correctly stopped that
path from moving the stored epoch, but nothing stopped it being *reached* on a
stale snapshot and rewriting a superseded secret.

**The second step closed it** (`9a0f539`): one per-`(account, group)`
**serializing** lock spanning the whole read-decide-write, **shared** across all
four entry points — receive, rotate, ensure, redistribute. With it, the argument
below is finally the argument the device actually runs: the gate re-reads inside
the lock, so a stale re-broadcast meets the **current** `(secret, mint_epoch)` and
is refused `reject-stale`. Web and Android were driven into the failure before the
fix and observed to produce it (signature
`expected 'accept-identical' to be 'reject-stale'`); on iOS the batch race was
**refuted rather than fixed** — the read-to-persist span has no suspension point
and `PushRoutingStore` has no `async`, so `@MainActor` already excluded it — but
the lock was added anyway because that safety was incidental and one added `await`
would have reopened it silently.

**The qualification, stated precisely: closed for the SINGLE-CONTEXT case.** The
lock serializes within one JS realm (web), one app process (natives), and — since
the follow-up finding was fixed — across tabs on web wherever `navigator.locks` is
available. The remaining boundaries are the re-scoped **residual 10**.

The `lakko-security` review of the lock returned **APPROVE WITH NOTES**, no
CRITICAL / HIGH; both of its actionable findings (a per-realm rather than cross-tab
web lock; the lock held across unbounded network I/O) are closed.

What the arc built, all of which now stands:

- **The envelope went v2, a hard flag day** —
  `domain(29, unchanged) ‖ version(1)=0x02 ‖ mint_epoch(8, BE u64) ‖ secret(32)`,
  **70 bytes**. The 29 domain bytes are the `incoming_envelope` dispatch prefix and
  are frozen forever, so the in-payload version byte moved instead. **No
  dual-accept**: a retired v1 envelope is refused with `UnknownVersion`, because a
  v1 envelope carries **no `mint_epoch` and therefore has no position in the
  order** — admitting one would re-open this exact residual for precisely the
  envelopes a stale device sends. A mixed-version fleet fails closed to **defer**
  in both directions, never to the leaky raw `group_id`, so invariant #4 held
  through the flag day.
- **The gate gained the missing predicate.** Envelopes are ordered on the pair
  `(mint_epoch, secret_bytes)` — epoch first, then the 32 secret bytes
  lexicographically, **strictly greater wins, equal is not newer**. The order is
  defined **once**, in `crates/lakko-contacts` (`push_routing_secret_is_newer`),
  and consumed by all three clients over the FFI (`pushRoutingSecretIsNewer`);
  none re-implements it, because three clients each hand-rolling a comparison is
  exactly the drift class that produced this finding.
- **Why this specific scenario now fails, as intended.** `S_old` was minted at
  some epoch `E_old` no later than the pre-removal epoch; the rotation to `S1` is
  broadcast **strictly after** the Remove commit (**C-61**, now test-pinned — see
  residual 6), so it is stamped `E_rot >= E_remove > E_old`. The re-broadcast of
  `S_old` is therefore **strictly less** in the order, and a member holding `S1`
  refuses it as `reject-stale`: no persist, no subscribe, no teardown of
  `gid(S1)`. The load-bearing clause is "a member holding `S1`" — the gate must
  read `S1`, not the pre-batch snapshot it replaced, which is why the serializing
  lock (**C-62 sub-invariant 5**) is part of the closure and not an
  implementation detail alongside it.
- **The stamp is persisted and re-emitted verbatim.** All six re-broadcast sites
  recompose from the *stored* secret, so a site that re-stamped the *current* epoch
  at send time would make the stale envelope arrive **newer** than the live
  rotation and reproduce this residual with the ordering gate fully in place. Only
  a mint reads `Group::epoch()`. Android makes this impossible to break BY ACCIDENT:
  `composeEnvelope` takes a sealed `MintStamp` whose `Fresh` / `Replayed`
  subtypes have `internal` constructors with one producer each in `src/main`.
  The claim is bounded and should not be read as stronger — Kotlin `internal` is
  **module**-scoped and the gate, the repository and the chat client share the
  `:app` module, and `StoredSecret`'s constructor is public, so a wrong stamp is
  still *constructible*. What it removes is the **silent** failure mode: a bare
  `ULong` cannot be passed where a stamp is expected, so the mistake now takes a
  differently-named, reviewer-visible call.
- **A future clamp guards the new capability the ordering created.** Without it an
  authorised writer could stamp `u64::MAX` and **freeze** the group's routing id
  permanently, escalating **residual 3** from admin-can-churn (self-repairing at
  the owner's next rotation) to admin-can-freeze (which does not). The clamp
  bounds an incoming stamp against the receiver's own group epoch and is evaluated
  **before** authority, so not even the owner can freeze the group; it never
  rejects an honest envelope, since `stamp <= seal epoch <= receiver's epoch at
  dispatch`.
- **A related defect was found and fixed during implementation.** `accept-identical`
  short-circuits **both** the authority gate and the clamp, so re-persisting the
  *incoming* epoch on that path let a **plain member** relay the group's current
  secret bytes stamped `u64::MAX`, inflate the stored stamp, and refuse every later
  rotation including the owner's — reaching the very freeze the clamp prevents,
  **around it**. All three clients now preserve the **stored** stamp on that path,
  each with a named regression test that also proves a later owner rotation still
  lands. The general rule: **a rule that short-circuits a gate must not write any
  state that gate protects.**
- **Fix direction 2 turned out to be already structural** on all three clients and
  is **pinned rather than rebuilt**: every store writes the secret, the
  `distributed` flag and the stamp in **one atomic write**, so accepting a
  different secret replaces the bytes and clears the pending marker together. It
  remains **complementary, not superseded** — it removes the trigger, ADR-0044
  removes the effect.
- **Accepted narrowing.** A total order and unconditional owner-priority cannot
  both hold: the owner still wins whenever it mints at a later epoch (the normal
  case), but **at an exact epoch tie the bytes decide and the owner can lose to an
  admin**. The property owner-priority existed to provide — arrival-order
  independence — is preserved by a strictly stronger mechanism, and both tied
  values are authorised post-removal mints equally unknown to the evicted member.
- **Not closed by this:** the order stops a stale value **spreading**; it does not
  repair a device already holding one (see residual 7).

#### Residual 10 — the receive gate's read-modify-write is serialized per PROCESS-CONTEXT; the cross-context boundaries are not all covered (ADR-0044 review; originally HIGH and blocking — **the in-process race is CLOSED, the entry is RE-SCOPED, not deleted**)

**Original finding (CLOSED).** Found by the `lakko-security` review of the landed
ADR-0044 arc — which returned **REQUEST CHANGES** and **reproduced the defect by
execution** on web, driving two envelopes concurrently the way `Chat.tsx` actually
drives them and observing verdicts `['accept-admin-rotate', 'accept-identical']`
with a final stored secret of `0xEE` **at epoch 10**, the stale one. It was
recorded at **HIGH** because its consequence is the same **confidentiality**
property residual 9 is about, not wake delivery.

**The shape it had.** The receive gate is a **read-modify-write**: it reads the
stored `(secret, mint_epoch)` pair, computes a verdict against it, then persists.
There were **suspension points between the read and the write**, and **no client
held a per-`(account, group)` lock across the sequence** — web dispatched it
`void`-prefixed and fire-and-forget from a batch loop, Android launched it per
envelope on `pushScope` (`inflight` guarded `ensure`, `rotatingLocks` guarded
`rotate`; neither covered receive), iOS spawned a detached `Task`. So in the exact
journal order F-A describes — a device holding `S_OLD@10` polling
`[rotation S1@11, stale S_OLD@10]` — gate **A** accepted the rotation and persisted
`(S1, 11)`, while gate **B**, reading the same obsolete snapshot, found its
incoming bytes byte-identical, resolved `accept-identical` (which **by design**
short-circuits both the clamp and the order), and rewrote the superseded secret one
turn behind A. The ADR-0044 § 5 hardening did not save it: B files the *stored*
epoch it read, the stale `10`. **The gate was bypassed, not defeated** — nothing
here ever said the order was wrong.

**How it was closed** (`9a0f539`): one per-`(account, group)` **serializing**
(never coalescing) lock spanning the whole read-decide-write, **shared** across
**four** entry points — receive, rotate, ensure, **redistribute**. Redistribute was
included beyond the minimum because it is the same read-then-write and
unserialized it puts a **PRE-rotation** secret back on the wire mid-rotation.
Sharing **one** lock rather than several also closed **residual 5 / F-E's
rotate-vs-receive interleaving**, since rotate and receive now exclude each other
rather than each merely excluding itself.

Verified by execution on **web and Android**, both of which reproduced the defect
before the fix with the signature
`expected 'accept-identical' to be 'reject-stale'`; on Android the concurrency is
genuinely **parallel** (`pushScope` is `Dispatchers.IO`), so a confined dispatcher
would have masked it, and Android additionally **proved the lock must be SHARED**
by probing with receive on its own lock — F-A and the N-rotation case passed while
**F-E still failed**. On **iOS the batch race was REFUTED, not fixed**: the span
from the `getRecord` read to the persist contains no suspension point (both
`await`s sit after the write) and `PushRoutingStore` has no `async` at all, so
`@MainActor` already made it mutually exclusive. The lock was added regardless,
because that safety was **incidental** and one added `await` would have reopened it
silently. iOS was not defect-free: its rotation *does* park in its broadcast, so
the rotate-vs-receive interleave (**F-E**) was real there.

**What REMAINS, and why this entry is re-scoped rather than deleted.** A lock
serializes within one **process context**. Two boundaries are therefore still in
scope, one handled and one unexamined:

1. **Web, cross-tab — handled, with a degradation mode.** `routingRowQueue` is
   module-level state, so it orders callers within one JS realm, while
   `lakko-push-routing` is an IndexedDB database shared by **every tab on the
   origin**. That was raised as a **MEDIUM** by the review of the lock and is
   fixed: a **Web Lock** (`navigator.locks`) of the same per-row key nests inside
   the in-process queue. **But `navigator.locks` is secure-context only**, so a
   browser without it — an insecure-context origin, an old browser, an embedded
   webview — **degrades to same-tab-only serialization, silently**. That is the
   live remainder on web. (The in-process queue is deliberately **kept** rather
   than replaced precisely because this state is real: with one mechanism its
   absence would be an invisible unserialized run. The queue also captures
   ordering synchronously, so a burst in one turn runs in DS journal order.)
2. **Natives, cross-process — NOT EXAMINED by this arc.** The iOS NSE and the
   Android background decrypt (ADR-0041 / C-59) run in a different process from
   the foreground app and touch the same on-device storage. Whether either ever
   reaches the routing row's **write** path — as opposed to the read-only
   `push_gid -> (account, group_id)` reverse-map lookup the background decrypt is
   documented to do — was **not established either way** here. Recorded as an open
   question, not as a cleared one.

Two smaller lock-scope questions **were** examined and deliberately left alone, on
web: `clearAllGroupRouting()` (`AuthContext.tsx`, on `forgetDevice` / logout)
writes the store **outside** the lock, judged safe because it is a **whole-store
teardown** rather than a per-row read-modify-write — it cannot lose an update or
decide against a stale snapshot, and the state it leaves is the
no-secret / no-record case every entry point already handles defensively; and the
two service workers touch the same database but only the `routes` store and only
in **`readonly`** transactions, so they are not a party to the sequence the lock
serializes.

#### Two loose ends found during the arc

- **~~Nothing reads the web `?wake=` deep-link parameter.~~ CLOSED.** The service
  worker built `/?wake=<hex>` on notification tap and its comment claimed the SPA
  routed "via its own deep-link reader (set in 1.18g)" — but **no reader existed**,
  so a web notification tap had **never** opened the target chat; it landed on the
  app shell. (The routing id itself was fine: ADR-0042 threads the reverse-resolved
  real `group_id` hex into `tapGroupHex`.) Now closed by
  `apps/web/src/push/{wake-target.ts,WakeNavigator.tsx}`: the cold-open `?wake=`
  param and a new `LAKKO_NOTIFICATION_TAP` message (the already-open case had no
  usable click-time signal at all — the SW only called `focus()`) feed one reader
  that navigates once the session **and** identity secrets are restored, holding
  the target across the login bounce. It only ever navigates to a group already in
  the in-memory registry — an attacker-supplied `?wake=` can never make it fetch,
  subscribe, or create anything — and a malformed / unknown / still-opaque
  `push_gid` value is dropped silently. The stale SW comment is corrected. The
  Android tap deep-link already worked (`7da84ee`).
- **~~`unsubscribeGroup` is dead code on all three clients.~~ CLOSED** (2026-08-11,
  ADR-0043) — the receive-side teardown is its first real caller on every client;
  see Residual 2.

**Status: OPEN (accepted residuals).** **Residual 1 is the substantive privacy
limit and survives ADR-0043 intact** — it is inherent to *any* group-shared routing
id, not merely a stable one: rotation renames the cluster, the provider re-links
across the boundary by token-set intersection, and the set difference names the
evicted device. Closing it needs per-device-blinded ids. **Residual 2 is CLOSED**
(rotation on removal + live per-group unsubscribe). **Residual 3 is WIDENED** from
admin-seed to admin-churn by the ratified option (b); the `lakko-security` pass
ADR-0043 declares mandatory for (b) **was run and returned APPROVE-WITH-NOTES, no
CRITICAL/HIGH**, its one blocking MEDIUM (F1, the coalescing rotation lock) fixed.
**Residual 4 is unchanged** and gains a second clause (such groups now also never
rotate). **Residuals 5-9 were added by ADR-0043. ADR-0044 closed residual 6 outright, and —
once the serializing lock its own review demanded had landed (`9a0f539`) — closed
residual 9 and the concurrent-rotator core of residual 7 for the
single-context case, plus the rotate-vs-receive half of residual 5. Residual 10,
minted by that review, is re-scoped rather than closed.**

**Residual 6 is CLOSED** ([ADR-0044](../adr/0044-push-routing-secret-recency.md),
`936d849` / `014e84e`) — C-61 now has a regression test on all three clients, each
asserting on the emitted envelope's stamp and each carrying a permanent
inverted-order companion, with the discrimination **proven by running the swap** on
both natives; ADR-0044 also **sharpened** what a C-61 regression costs, from the
rotation's forward secrecy alone to that **plus an inverted recency comparison**
that turns "the rotation always wins" into a silent coin flip.

**Residual 9 is CLOSED for the single-context case.** ADR-0044 built the general
fix its own entry prescribed — a v2 envelope carrying a `mint_epoch`, a canonical
`(mint_epoch, secret_bytes)` total order defined once in `crates/lakko-contacts`
and consumed over both FFI surfaces, a persist-and-replay-verbatim rule, and a
future clamp evaluated ahead of authority — and the `lakko-security` review of the
landed arc nonetheless **reproduced the defect by execution**, returning **REQUEST
CHANGES**: the primitive was correct, but the receive path that consumed it was an
**unserialized read-modify-write dispatched fire-and-forget per envelope**, so the
gate decided against a **stale snapshot** and was **bypassed, not defeated**
(**residual 10**). The serializing lock in `9a0f539` closed that, and the review of
the lock returned **APPROVE WITH NOTES**, no CRITICAL / HIGH, with both actionable
findings (the per-realm web lock; the lock held across unbounded network I/O) since
closed. **Residual 7's concurrent-rotator core closes on the same fix and with the
same qualification.** Its **F-D stale-role window** is untouched regardless (an
authority refusal, on which recency has no bearing), an ordering gate **does not
repair an already-stale holder**, and the N+1 rotations and their churn still
happen.

Still open, then: **residual 5** (the orphaned old gid — its **rotate-vs-receive
half is CLOSED** by the shared lock; what remains is the transient store-read
failure and the crash case, accepted and inert), **residual 7's remainder** (the
F-D stale-role window, the no-repair limit, and the N+1 churn itself), **residual
8** (the web teardown-before-subscribe window, F4), and the **re-scoped residual
10** (no longer HIGH or blocking: the in-process race is closed and web's cross-tab
boundary is handled where `navigator.locks` exists; what remains is the
insecure-context degradation to same-tab-only serialization, and the
native cross-process boundary this arc did **not** examine). The `lakko-security`
pass on option (b) **is done — APPROVE-WITH-NOTES, no CRITICAL/HIGH, no invariant
#2/#3/#4 break on the live path** — with its one blocking MEDIUM (F1, the
coalescing rotation lock) fixed on web + iOS by adopting Android's serializing
lock.
Invariants: #2 holds
(push-gw is byte-identical unchanged across both ADRs and handles the same opaque
`BINARY(32)`; the secret rides an E2EE MLS application message), #4 holds (defer
rather than downgrade — a member without the current secret does not subscribe at
all; the receive gate still rejects on an unreadable role table or an unresolvable
sender), #5 is **improved but not fully satisfied** — the server no longer
materialises a *DS-linkable* social graph, and the cluster is no longer
*forward*-linkable across a removal by the evicted party, but an opaque
co-membership cluster is still materialised and still re-linkable by intersection.

## Dependency advisories

The `cargo audit` baseline (RustSec advisory DB) surfaces
two open vulnerabilities + three unmaintained-crate
warnings in the workspace's transitive dependency tree.
None are in code we control; remediation depends on
upstream releases. Each entry below lists severity,
reachability analysis, and the upstream tracker the fix
is gated on.

### G-23: `libcrux-chacha20poly1305 0.0.7` — RUSTSEC-2026-0124 (HIGH 8.2) — **CLOSED (fixed)**

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

### G-24: `rsa 0.9.10` — RUSTSEC-2023-0071 (MEDIUM 5.9) — Marvin Attack — **CLOSED (Jun 2026)**

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

### G-25: Unmaintained-crate + advisory warnings — **all CLOSED (2026-07): the three unmaintained warnings + two 2026 transitive advisories (anyhow, crossbeam-epoch); `cargo deny check advisories` is clean**

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

### G-41: three `libcrux` advisories under the MLS crypto provider - **OPEN as a `cargo deny` failure; NOT reachable in Lakko's build (analysed 2026-09-22)**

`just audit` reports three vulnerabilities, all in one dependency
chain and none in code Lakko controls:

| Advisory | Crate | Fixed in |
|---|---|---|
| RUSTSEC-2026-0212 | `libcrux-secrets 0.0.5` | `>= 0.0.6` |
| RUSTSEC-2026-0207 | `libcrux-sha3 0.0.8` | `>= 0.0.10` |
| RUSTSEC-2026-0208 | `libcrux-sha3 0.0.8` | `>= 0.0.10` |

The chain is `libcrux-secrets <- libcrux-traits 0.0.6 <- libcrux-sha3
0.0.8 <- hpke-rs 0.6.1 <- openmls_rust_crypto 0.5.1 <- lakko-mls`, so
this sits under the MLS HPKE backend every client ships, and
RUSTSEC-2026-0212 is a constant-time failure on **aarch64** - every
iPhone and most Android devices. That is why it was worth reading
rather than waving through.

**Reachability: no, and by three independent arguments.** Any one of
them is sufficient; all three were checked against the resolved graph
for `aarch64-apple-ios` and `aarch64-linux-android`.

1. **The broken primitive has no callers.** RUSTSEC-2026-0212 is in
   `Select::select` / `Swap::cswap`, whose aarch64 bodies do
   `cmp {cond:w}, 0` on an 8-bit selector widened to a 32-bit
   register - the unspecified high 24 bits can make a zero selector
   compare non-zero, so `csel` picks the wrong operand
   (`libcrux-secrets-0.0.5/src/int.rs`, `mod aarch64`). Those two
   traits are **defined in `libcrux-secrets` and named nowhere else**:
   no crate in the dependency set calls `.cswap(`, and every crate
   that imports from `libcrux_secrets` takes only the type-level
   wrappers (`Classify`, `ClassifyRef`, `Declassify`, `DeclassifyRef`,
   `DeclassifyRefMut`, `U8`). The only glob importers are
   `libcrux-ml-kem`, which is not in the graph.

2. **`libcrux-sha3` is only called from the post-quantum arms.**
   `libcrux_sha3::shake256` has exactly two call sites in `hpke-rs`
   (`src/kem.rs`, `derive_key_pair`), both inside the
   `XWingDraft06` / `MlKem768` / `MlKem1024` match arms. Lakko's four
   ciphersuites
   ([`crates/lakko-mls/src/ciphersuite.rs`](../../crates/lakko-mls/src/ciphersuite.rs))
   are DH-KEM over X25519 / P-256 / P-384 and take the preceding arm.
   No Lakko source names SHA3, SHAKE or a PQ KEM anywhere.

3. **The libcrux HPKE backend is not compiled at all.** `hpke-rs`
   gates both backends behind optional features (`libcrux` /
   `rustcrypto`), and `cargo tree -i` resolves `hpke-rs-rust-crypto`
   while `hpke-rs-libcrux`, `libcrux-aead`, `libcrux-kem`,
   `libcrux-hkdf`, `libcrux-ecdh` and `libcrux-curve25519` are all
   absent from the graph on every target checked. `libcrux-sha3` is a
   NON-optional dependency of `hpke-rs`, which is the only reason any
   libcrux crate is present.

So the vulnerable code is compiled on aarch64 and never executed.
**This is an argument from the current resolved graph, not a
guarantee**: adopting a PQ or hybrid ciphersuite would make argument 2
false, and a feature-unification change upstream could make argument 3
false, so this reasoning must be re-run whenever the MLS crypto
provider or the ciphersuite set moves.

**Why it nevertheless stays OPEN.** `cargo deny check advisories`
fails on it, so `just ci` is red. The clean-audit posture the G-25
entry argues for is a real property worth keeping: a red gate that
everyone knows to ignore stops being a gate. The fix is not
available, though - see below - so the choice is between a red
`just audit` and an `[advisories] ignore` entry pinned to these three
IDs with a pointer here.

**Why `cargo update` cannot fix it.** `libcrux-traits 0.0.8` already
pins the FIXED `libcrux-secrets = "=0.0.6"`, so the vendored-manifest
idiom that closed G-23 looks like it should reach it - and does not,
because of an exact-version knot one level down: `libcrux-secrets
0.0.6` requires `hax-lib =0.3.7` while `libcrux-sha3 0.0.8` requires
`hax-lib =0.3.6`, and both pins are exact. Bumping the traits alone
fails to resolve (verified).

**The route, unbuilt.** Both lines have to move together:
`libcrux-traits 0.0.7 -> 0.0.8` in the existing vendored
[`crates/vendor/hpke-rs-libcrux`](../../crates/vendor/hpke-rs-libcrux/Cargo.toml),
plus a SECOND vendored crate - `hpke-rs 0.6.1` with its
`libcrux-sha3 ^0.0.8` requirement relaxed to `0.0.10` - since
`hpke-rs`, not the libcrux backend, pins the sha3 line. That is a
fourth `[patch.crates-io]` entry on the MLS crypto graph, for code
argument 1 says is never called. The alternative is upstream:
`openmls_rust_crypto 0.6.0` and `hpke-rs 0.7.0` both exist, and moving
to them is an openmls bump rather than a manifest delta.

**Side finding, unresolved.** The G-23 vendored `hpke-rs-libcrux` fork
patches a crate that is **not in the resolved graph on any target
checked here** (argument 3), so that fix may be a no-op in the current
feature set. It may have been reachable when it was made, and this was
not investigated - recorded so the next audit pass does not assume the
patch is load-bearing.

**Two advisories from the same run WERE closed** by plain lock bumps,
with `just lint` and the 2779-test workspace suite green afterwards:
`h2 0.4.13 -> 0.4.19` (RUSTSEC-2026-0258, unbounded empty DATA frames)
and `rustls 0.23.40 -> 0.23.45` with `rustls-webpki 0.103.13 ->
0.103.15` (RUSTSEC-2026-0285, TLS 1.3 handshake messages accepted
across encryption-level boundaries - which matters here because the
whole anti-censorship stack of
[ADR-0026](../adr/0026-anti-censorship-wiring.md) is built on rustls).

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

### G-37: The Android chat repository held ONE repository-wide lock across a deliberate 30-second DS long poll — **CLOSED on Android (2026-08); residuals recorded below**

**The original framing of this entry was wrong, and the correction is the point.**
It was filed (2026-08-14, out of the ADR-0044 lock-bounding work) as a *resilience*
hazard: "a hung DS request wedges the shared lock indefinitely". The code was worse
than that — it was the **steady state**, no hang required. `ChatRepository.groupsLock`
was one `Mutex` per **repository**, not per group (its own doc comment said so and
even named the fix), and `pollGroup` awaited a **deliberate 30-second DS long poll
while holding it**, driven by an unbounded `while (isActive)` loop on the open chat
screen. So while any chat screen was open, that single global lock was held
near-continuously and every send, edit, reaction and membership commit **on every
group** queued behind it. The chat-list screen then polled groups serially, making
the worst case `waitSecs * N`.

**The fix is in two parts, and neither is "add a timeout".**

**1. The poll is restructured, not bounded.** A timeout here would sever a long poll
that is behaving exactly as designed. `pollMessages` was split into a network half
(`ChatClient.fetchGroupJournal`) and an MLS half (`ChatClient.applyFetchedMessages`),
the latter deliberately **not `suspend`** — a structural guarantee that a network
wait cannot be reintroduced into the critical section, because a suspending call
would not compile there. The fetch runs outside the locks; the apply and the cursor
advance run inside, atomic with respect to each other. The cursor is read before the
fetch and re-read under the lock: if it moved, the batch is **dropped unapplied**
(`SplitPollOutcome.SKIPPED_CURSOR_MOVED`), because applying it would re-feed already
consumed rows to OpenMLS and — worse — let the stale batch's end cursor move the
record's cursor **backward**. A per-group in-flight latch stops two polls of one
group overlapping, released in a `finally` so a failed or cancelled fetch never
wedges polling. See `apps/android/app/src/main/kotlin/me/lakko/app/chat/SplitPoll.kt`
and its host tests.

**2. DS I/O is bounded at the TRANSPORT, not with `withTimeout` — and that choice is
load-bearing, not tidiness.** A `withTimeout` wrapped around a commit site would put
a `TimeoutCancellationException` **inside** `runCrashSafeCommit`, where the
persist-catch would swallow it and fire `resetGroupToDurableSnapshotOrThrow` —
silently converting a network timeout into a commit rollback, i.e. manufacturing the
device-behind outcome [ADR-0040](../adr/0040-durable-pending-commit-outbox.md) exists
to prevent. Bounding `DsClient` instead means no such exception is ever constructed:
Ktor's `HttpTimeout` throws `HttpRequestTimeoutException`, which extends
`java.io.IOException`, so it lands in the existing `DsNetworkError` path and reaches
callers as `ChatError`. Nothing configured a timeout before, so the effective budget
was inherited and unknown; it is now explicit (15 s request + socket), and a
long-poll fetch widens **both** request and socket to `waitSecs + 10` — the socket
deadline has to move too, since a long poll is an idle socket by definition.

**Then the lock itself was narrowed to two tiers — and THIS correction is the part
worth reading.** The obvious fix is the one the old doc comment suggested: swap the
single mutex for a `ConcurrentHashMap<String, Mutex>` keyed per group. **A straight
per-group swap would have broken ADR-0040.** One OpenMLS provider serves *every*
group, and the FFI offers no per-group persistence:

- `Identity.snapshotStorage()` serialises the **whole** provider under a single read
  lock, while one logical MLS operation performs several separate writes into it
  (`MemoryStorage` takes its lock per write). A snapshot taken by group A part-way
  through group B's multi-write operation captures **B torn**, and that torn blob is
  what a later restore or restart loads.
- `Identity.restoreStorage()` — the ADR-0040 F1 abort path,
  `resetGroupToDurableSnapshotOrThrow` — replaces **every** group's state at once.
  Running it while another group sits at an un-persisted N+1 would **roll that group
  back**, re-opening the exact device-behind window ADR-0040 closes.

Both were impossible before only because ONE mutex covered every group — an
accidental safety the narrowing would have removed. So the shape is **two tiers**: a
per-group `Mutex` (outer, spanning the whole operation including the publish, so
same-group ordering is byte-for-byte what it was) plus a **global `providerLock`**
(inner, held only over provider-touching regions). Ordering is always per-group
first, provider second, never the reverse, and never two per-group locks at once.

**Where the concurrency actually comes from is the lifting, not the narrowing.** Each
path now splits into a provider half and a network half — `ChatClient.prepareSend` /
`publishPreparedSend`, `prepareCrashSafeCommit` / publish, `fetchGroupJournal` /
`applyFetchedMessages` — so what different groups serialise on is local CPU plus one
snapshot write, never a 15 s DS publish or a 30 s long poll. The ADR-0040 §2 ordering
survives the split **structurally**: `prepareCrashSafeCommit` returning normally IS
the signal that both durable writes landed, and the publish is a separate call that
cannot run until it has returned, so a throw still means the publish is provably
un-run.

**`pollAllGroups` is now concurrent, bounded at 3.** The bound is not about CPU. Every
DS long poll goes to the same host and Ktor's OkHttp engine dispatches through
`Call.enqueue`, so OkHttp's `Dispatcher.maxRequestsPerHost` applies — default 5.
Filling all five slots with idle long polls would queue the user's next send behind
them in `readyAsyncCalls`, trading lock contention for connection starvation, which
no amount of lock narrowing could then fix. Three leaves at least two slots free (the
open chat screen's own continuous long poll, and one interactive publish) while
collapsing chat-list latency from `waitSecs * N` to `waitSecs * ceil(N / 3)`. The
fan-out is a `supervisorScope` with a per-leg `try/catch` — a plain `coroutineScope`
would propagate the first child failure and cancel its siblings, which is strictly
worse than the serial loop it replaces; `CancellationException` is re-thrown so a
cancelled poll stays cancelled.

**Residual, stated honestly.** Cross-group operations are **not** fully independent.
They still serialise on the provider region — local CPU plus one snapshot write — and
that is a direct consequence of the FFI's whole-provider persistence granularity, not
of the locking. What is now **outside** it is everything slow: the 15 s publish and
the 30 s long poll. Making groups genuinely independent would need per-group
persistence in `lakko-mls` / the FFI, which is an MLS-core change (invariant #3) and
is not attempted here.

**Two side-effects of removing the accidental global lock, recorded because they were
easy to miss.** `groups` is a `Collections.synchronizedMap`, whose *iterators* are not
self-synchronising; the repository-wide mutex made unsynchronised iteration
accidentally safe, so `persistMessageLogBestEffort` (and the group-id snapshot in
`pollAllGroups`) now take the map's own monitor explicitly. And the per-group mutex
map is cleared in `wipeInMemoryState` alongside the records it guards, so an account
switch does not carry a previous session's locks.

**The question this entry left open for the other two clients has been answered, and
the answer lives in G-38 below.** The original text said the same question had *not*
been asked of the iOS and web chat
repositories. It has now: they had the **opposite** exposure — no serialization at
all — and the residual "lock held across an unbounded network call" survives there in
a much smaller, per-group form, recorded under G-38 rather than duplicated here.

**Severity, unchanged in kind:** availability. No confidentiality or integrity
property was ever weakened by the lock being held — nothing about who may write, or
which write survives, changed. What was lost was liveness on a shared lock, and on
one path (ADR-0044's routing row, since insulated by `boundedOrNull`) liveness was
what a confidentiality guarantee rested on.

### G-38: Two membership commits could be in flight to the DS at once → inverted journal order → device-ahead — **serialized on web + iOS + Android and the local poll-vs-commit interleave closed on all three (2026-08); the remote-commit epoch fork is CLOSED for the member membership paths by ADR-0045 and, since S7 (`576ea14`), CLOSED for the external-join path too — the last direct-commit route in a role-table group is gone**

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

### G-39: The external cryptographic review of the MLS state machine has not happened — **OPEN, and it is a release gate**

**Owner:** [ADR-0046](../adr/0046-external-crypto-review.md), which moved this obligation
here from where it had been scattered — two ADRs stating it as a blocker
([ADR-0040](../adr/0040-durable-pending-commit-outbox.md),
[ADR-0045](../adr/0045-remote-commit-epoch-fork.md)), four `C-` rows in
[`invariant-index.md`](./invariant-index.md) repeating it, and four further ADRs
mentioning it without acting on it. It is stated **once**, in this entry, and nowhere
else.

**What it is.** Security invariant #3's third clause: *"(after Phase 2) an external
crypto audit"* for changes to `lakko-mls`. The per-change half of that invariant — an ADR
plus property tests, before the change lands — is unchanged and is still enforced per
change. This is the other half.

**Why it is a gap rather than a blocker on any one document.** Carried inside each
MLS-touching ADR, it made every one of them permanently un-Acceptable regardless of how
finished it was, while specifying nothing an auditor could act on. A blocker that blocks
unconditionally and forever stops being read. ADR-0046 has the full argument.

**The gate, stated so it cannot be quietly routed around: no build reaches users while
this is open** — no store submission, no GA tag. That is where "after Phase 2" bites.

**Scoped for an engagement in
[`external-review-scope.md`](./external-review-scope.md)** (added 2026-09-02) — the
boundaries, the measured sizes, a reading order through ten thousand lines of security
documentation, what has already been verified and where that verification is thin, the
six questions ranked by where we think the risk is, and the five things a reviewer must
not assume. This entry states the obligation; that document states the engagement.

**The surface to be reviewed.** Larger than any one ADR, because the interactions are the
point — a per-change review would see each of these as a slice:

- **ADR-0045's core**, six sprints: the per-proposal authorisation gate (`authorise`, one
  rule at three layers, by-reference judged against the proposer and by-value against the
  committer); `owner_survives` (C-63); the staged sweep's stage / merge / abandon
  lifecycle and its per-group rollback; the `create_application_message` pending-set park;
  the committer election over the `0xFF02` role table; and S7's approval-as-proposal.
- **The invariants that arc minted or amended:** C-63 through C-69 — in particular C-66
  (the reserve past the pending cap, and the sweep's fifteen enumerated build conditions
  plus the exclusion search that backs them), C-67 (the cursor-may-lag-never-lead ordering
  rule, and the owed-artifact lifecycle) and C-68 (author identity as a signature key
  resolved at receipt).
- **ADR-0022's role table** and its `0xFF02` GroupContext extension, which everything
  above is layered on.
- **Three independent client implementations** of the ordering and rollback disciplines
  the core change depends on — the send lock, the verdict apply, the sweep adjudication,
  the durable-cursor ordering. These are not incidental: C-69 exists because a discipline
  can be correct in the core and attached to the wrong end in a client.
- **The `lakko-security` findings and their fixes** (2026-08-29, seven findings), which
  are the most recently changed part of the surface and therefore the least settled.

**What an internal review already found, as an argument for the external one rather than
against it.** The `lakko-security` pass on an already-shipped surface returned three HIGH
findings plus a confirmed impersonation. **None needed an attacker and four needed nobody
at all** — a network blip, a stale roster, two admins doing an ordinary thing, or enough
time passing. An external reviewer does not share the blind spots that let those ship.

**Residual risk while this is open.** Everything above is peer-verified and
property-tested but unaudited by anyone outside the project. The specific classes an
external review is best placed to catch, and which the work so far has *not* systematically
covered: cross-invariant interactions (each was reasoned about locally); the enumeration
in C-66 being a mirror of openmls rules Lakko does not own; and whether the three-layer
gate's by-reference/by-value split holds under compositions nobody has thought to write a
test for.

## Delivery service

### G-42: A journal's name is its only key - a leaked `group_id` reads, writes and knocks; a mailbox is named by a public key anyone can compute - **CLOSING under [ADR-0053](../adr/0053-ds-journal-authorisation.md): S1-S4 built and deployed to the test stack 2026-09-27; a claimed journal refuses a request by name alone (walked live: unsigned read, unsigned write and a forged proof 401, a competing registration 409). Residuals: rendezvous mailboxes stay open by name by design, an evicted member keeps READ access to ciphertext until its epoch ages out of the retention window, and the S5 `lakko-security` pass is open**

**Symptom.** `services/ds` serves and accepts both journals to anyone who names them
(`services/ds/src/main.rs`: *"No authentication on Phase 1.3 - anyone with the right
pseudonym can publish or fetch"*). Three roads hand the name to people outside the
conversation: (1) a forward carries the origin conversation's `group_id` inside its
sealed payload ([ADR-0052](../adr/0052-reply-forward-pin.md) decision 2 - reproduced
live 2026-09-27: two recipients who share no conversation with the original author hold
the author-forwarder `group_id`); (2) a mailbox pseudonym is
`BLAKE3("lakko-pseudonym/v1/identity" || identity_pk)` and the identity key is
published by discovery, so a mailbox is derivable from a username; (3) the `/g/` joiner
reads the whole group journal and posts into it before it is a member, by design.

**What the name buys.** Not the content - every row is MLS- or HPKE-sealed. It buys the
metadata (row count, timing, sizes: the activity of a conversation, or whether and when a
person received a contact request or was added to a group), a write into the journal
(bytes every member fetches, fails to open and drops, at their cost), and for a group an
external-join proposal nobody invited ([ADR-0028](../adr/0028-group-join-ticket.md)
"ask, never in"). The pseudonym cannot be guessed (256 random bits), so every exposure is
one leaked name, one conversation - but ADR-0052 leaks one on purpose, and the mailbox
name was never secret.

**Why it went unrecorded.** The Phase 1.3 posture is stated in the service header and
relied on by [ADR-0045](../adr/0045-remote-commit-epoch-fork.md) (the case against a
DS-side epoch gate) and ADR-0028 (the joiner's unauthenticated read), but no gap named
what the posture costs once a name travels; ADR-0052 wrote *"an opaque conversation id
they cannot open; that is the whole disclosure"*, which is true of content and false of
the journal.

**Closing.** ADR-0053: a claimed journal is opened only by a request signed with a key
it was claimed under - for a group a per-epoch Ed25519 key every member derives from
`Group::export_secret("lakko/ds/access/v1", ...)` (rotation with the epoch, eviction
revokes at the commit), for a mailbox the identity key its pseudonym hashes; a failed
proof is answered as a nonexistent journal; the non-member's one act, asking to join,
moves to a PoW-gated knock whose response carries the epoch; the joiner's Welcome moves
to its mailbox (C-69 amended). Rendezvous mailboxes (pairing, recovery, courier, state
bundle: random names exchanged out of band, alive for minutes) stay capability-by-name
and are this gap's accepted residual. Until it lands: a leaked `group_id` is a read of
metadata and a write of garbage, and a forward from a one-to-one should carry no origin
(ADR-0052 amendment).


### G-43: `services/channels` authorised no publisher operation - anyone who knew a `channel_id` could rotate, deliver envelopes and read the subscriber list - **CLOSED in code 2026-10-01 ([ADR-0059](../adr/0059-channel-discover.md) section 9); not walked live** (recorded 2026-09-28 while designing ADR-0059)

**What the code does.** `create_channel` takes only `{channel_id}` and registers no key;
publishing a post, `POST /rotate`, `/rotate/stage`, `/rotate/commit`, delivering a seed
envelope, registering a subscriber and `GET /v1/channels/{id}/subscribers` take no
session, no signature and no owner key (`services/channels/src/api.rs`, the router;
`migrations/0001_init.sql`: `channels` is `id, current_epoch, next_cursor, created_at`).
ADR-0010's planned publisher-only distribute route was never built. Checked in the
tree on 2026-09-28; not measured against a deployment.

**What it buys an attacker.** Not post content: every post is AEAD-sealed under the
epoch seed and Ed25519-signed, and readers drop a post whose signature fails. It buys
availability and metadata: a rotation or a staged commit nobody authorised advances the
channel's epoch or files envelopes under it, which can leave subscribers without a seed
they can open; the full subscriber key list and its size are public; and, because
subscriber registration is free, the subscriber count is forgeable. A `channel_id` is
public to anyone who resolves the channel's handle.

**Closing.** ADR-0059 introduces a per-channel owner key registered at creation and uses
it for the directory, the subscriber list and a token-gated subscribe. Gating publish,
rotate and deliver on the same key (plus co-publisher keys the owner registers) is the
remaining change and is not part of ADR-0059.

**Closed (2026-10-01, ADR-0059 section 9).** `POST .../posts`, `POST .../rotate`,
`POST .../rotate/stage`, `POST .../rotate/commit` and `POST .../subscribers/<key>/envelope`
now go through `require_publisher` (`services/channels/src/owner_auth.rs`) before the body
is parsed: an owner proof, or a proof by a co-publisher key the owner appointed, over the
method, the service path, the query and the exact body bytes, inside the 120 s window,
nonce consumed once; every refusal is `401 owner-proof-required`. An unknown channel now
answers `401` instead of `404`. Web, iOS and Android sign all five writes and the subscriber
list with the role the device holds for the channel (an in-memory cache: owner on create,
co-publisher on a completed join, otherwise the first key the server accepts). Verified:
`services/channels` integration tests (42, including a refusal matrix per route: no proof,
foreign key, unappointed co-publisher, body mismatch, replay) and unit tests (72); the 6
SQL-store tests skipped without `LAKKO_CHANNELS_TEST_DATABASE_URL`; web vitest for channels and
discover (170); Android and iOS unit tests in scratch copies (this worktree does not compile
natively until the sealed-invite-link bindings are ported). Not exercised against a deployed
server. What remains open is the reads: posts, envelopes and the epoch stay readable by
`channel_id`, which is acceptable because posts and envelopes are sealed.


### G-44: Rejoining a call in the same MLS epoch reused SFrame `(key, nonce)` pairs - **CLOSED in the core, web, iOS and Android libraries by [ADR-0069](../adr/0069-sframe-random-counter-start.md) (2026-10-04); not seen on a live call**

Found by a `lakko-security` pass on 2026-10-04. A sender's nonce is `nonce_salt XOR counter`,
key and salt are a function of `(seed, KID)`, and a fresh `ParticipantKey` started its
counter at zero. The seed of a chat-group call is `export_secret("lakko/call/v1", call_id)`
of the chat group, which changes only with the MLS epoch. Hanging up and joining again,
reloading the tab or reconnecting in the same epoch therefore sealed new frames under
nonces already used, on every stream, on all three clients; a camera track that ended and
was published again did the same for the video stream on web and Android. The SFU holds
both ciphertexts: keystream reuse and a forgeable tag, a break of invariant #2 for those
frames. The earlier "V1 false alarm" note covered link calls only.

**Closed.** The counter is 64 bits and starts at a random value in `[0, 2^62)`; receivers
accept any 64-bit counter. Two keys under one `(seed, KID)` collide with probability at most
`(L1 + L2) / 2^62`. A flag day for call media. Touchpoints: `crates/lakko-calls/src/sframe.rs`
(`random_counter_start`, `compose_nonce`), the C ABI, both FFI crates, the C++ bridge, the
web worker. Property tests in `sframe.rs` (injective nonce, two keys of one seed never share
a counter, a frame opens under an independently derived key, exhaustion, the golden vector
still opens). Joins the surface of G-39.

