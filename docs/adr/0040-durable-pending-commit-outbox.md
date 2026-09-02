<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0040: Durable pending-commit outbox for crash-safe MLS group commits

- **Status:** **Accepted — implemented S1–S5 (2026-07), end to end on web + iOS +
  Android; G-35 CLOSED.** ~~Accepted — design (S1) … the S2–S5 per-client
  implementation is **pending**~~ — that status line stood after the
  implementation landed and is corrected here. The maintainer directed "design the
  ADR then build" on 2026-07-21; S1 is this ADR plus the G-35 known-gaps entry, and
  the outbox itself is `apps/web/src/chat/commit-outbox.ts`,
  `apps/ios/Lakko/Chat/CommitOutbox.swift` and
  `apps/android/.../chat/CommitOutbox.kt`. Option A (a durable pending-commit
  outbox, **no `lakko-mls` core / FFI change**) is the accepted mechanism; Option B
  (split create-from-merge in the core) is recorded as a possible future
  refinement, not chosen for v1. **Update (2026-08): Option B landed narrowly**
  under [ADR-0045](0045-remote-commit-epoch-fork.md) — for the new
  proposal-**sweep** path only, not for the direct-commit paths this ADR covers,
  and it does **not** retire the outbox. See the Open Questions entry. **Further
  update (2026-08-31): the outbox now also serves the ADR-0045 sweep** — each
  client's sweep adjudication is wired through the same `sweepPendingCommits` seam
  and outbox bookkeeping, so this ADR's mechanism carries a path it was not
  designed for. That is deliberate reuse, not drift: an owed sweep verdict and an
  un-acked direct commit are the same restart question.

- **Date:** 2026-07-23
- **Context:** Phase 3 — group-membership commits / MLS ratchet durability.
  Closes the G-34 T2 commit-path residual (the add / remove / role-change /
  external-join-approve paths share the same publish-before-persist ordering the
  chat send path had before G-34 T2, but with a **distinct, worse** failure
  mode — epoch desync, not same-generation nonce reuse).

> **A note on the line numbers in this document.** They are a 2026-07 snapshot and
> most of them have since shifted — `crates/lakko-mls/src/group.rs` roughly doubled
> under ADR-0045, and the client files moved with it. Where a citation was actively
> misleading (a wrong method **name**, a fabricated quotation, a line past a file's
> end) it is corrected in place below. The rest are left as they are and should be
> read as symbol pointers, per the anchor convention ADR-0045 adopted for exactly
> this reason.

## Context

The G-34 T2 fix (commit `ca318f3`) closed a **CRITICAL** chat-ratchet nonce reuse:
the application-message send path published the generation-N MLS ciphertext to the
Delivery Service (DS) **before** persisting the post-`createApplicationMessage`
(N+1) sender-ratchet snapshot, and the persist was best-effort (swallowed
failures). Because the OpenMLS provider is in-memory
(`crates/lakko-mls/src/identity.rs:56` — `provider: OpenMlsRustCrypto`, documented
"in-memory for Phase 0" at `identity.rs:49`), the snapshot is the **only** durable
ratchet copy, so a crash between publish and persist reloaded the ratchet at
generation N and re-consumed it: an identical AEAD `(key, nonce)` on a different
plaintext. The fix reordered every ratchet-advancing **send** to
`create → persist(durable, awaited, FAIL-CLOSED) → publish`.

That fix explicitly named a residual it did **not** address
(`docs/security/known-gaps.md:2823-2825`):

> Residual: the commit paths (add/remove/role/approve) share the same
> publish-before-persist ordering — a DISTINCT hazard (epoch desync /
> rolled-back-commit resend, not same-generation nonce reuse) worth its own
> separate analysis, not part of this fix.

**This ADR is that separate analysis.** It closes the residual for the four
group-membership **commit** paths.

### The four commit paths, and the eager merge

> **Amended 2026-08-31 (ADR-0045 S7, `576ea14`): there are now three, and in an
> ordinary chat group none of them is reached.** Two things moved under this
> heading and neither invalidates the analysis — the hazard is a property of the
> create-then-publish-then-persist *shape*, and every path that still has that
> shape still has the hazard.
>
> 1. **external-join approve is no longer one of them.** `approve_external_join`
>    publishes an `Add` **proposal** and returns proposal bytes
>    (`crates/lakko-mls/src/group.rs`, `approve_external_join` →
>    `propose_add_member`); it neither commits nor merges. The direct arm survives
>    as `pub(crate) approve_external_join_directly`, whose single caller is
>    `PublisherGroup::approve_external_join` and which refuses a group carrying a
>    role table.
> 2. **The remaining three are unreachable from a role-table group.** All three
>    clients branch on the role table (web's `usesProposalPath` in
>    `apps/web/src/contacts/client.ts`, and its two native twins) and route to a
>    proposal when there is one, and `Group::create` has seeded a table since
>    ADR-0022 — so the direct paths are now reached only by table-less groups
>    (`DeviceGroup`, `PublisherGroup`, call groups).
>
> The outbox is **not** thereby dead: it wraps both shapes. It still guards the
> direct paths where they are reached, and it is the seam the ADR-0045 sweep uses
> for its own restart-time verdict.

Every group membership change on all three clients ran the same shape: the
`lakko-mls` core **create-commit** call **eagerly merges** the epoch N → N+1
transition into the in-memory provider *inside* the create call, then the client
publishes the commit (and, for add / approve, a Welcome carried in a
`WelcomeRendezvous`) to the DS journal, then persists the N+1 snapshot
**best-effort** (failures logged and swallowed).

Core eager-merge sites (`crates/lakko-mls/src/group.rs`):

| Path | Core method | Eager merge |
|---|---|---|
| add member | `add_member` (`:269`) | `merge_pending_commit` at **`:280`** |
| remove member | `remove_member` (`:350`) | `merge_pending_commit` at **`:361`** |
| role change | `set_group_context_extensions` (`:377`) ← `set_group_roles` (`:547`) | `merge_pending_commit` at **`:392`** |
| external-join approve | `approve_external_join` (`:498`) → delegates to `add_member` at **`:516`** | (via `add_member` `:280`) |

The merge is **inside** the create call — the core returns already at N+1 in
memory, so there is no "created-but-unmerged" state a caller could hold.

Client call sites (each ends in a best-effort snapshot persist):

- **web** `apps/web/src/contacts/client.ts` — `persistBestEffort` is the
  final step of `addMemberToGroup`, `removeMemberFromGroup`, `setMemberAdmin` and
  `approveExternalJoin`. (The first two were cited here as `addMember` /
  `removeMember`; those methods do not exist and never did — corrected, since a
  wrong **name** sends a reader looking for the wrong thing, where a stale line
  number only sends them a few screens away.) The persist is best-effort by
  design — the class doc notes failures are "logged and swallowed; the in-memory
  state stays good."
- **iOS** `apps/ios/Lakko/Chat/ChatRepository.swift` — `persistSnapshotBestEffort`
  called from the add (`:627`), remove (`:696`), role (`:733`), approve (`:831`)
  paths.
- **Android** `apps/android/.../chat/ChatRepository.kt` —
  `persistSnapshotBestEffort` (`:1594`) called from the add (`:575`), remove
  (`:633`), role (`:669`), approve (`:790`) paths.

### The failure mode (deterministic, verified firsthand)

A crash **or** a silent best-effort persist failure in the window **between publish
and persist** produces a durable inconsistency:

- The commit C is **durable on the DS** (append-only journal) — peers poll it and
  advance to epoch **N+1**.
- The local snapshot **rolled back to N** (the N+1 snapshot never landed durably;
  the in-memory N+1 view is gone on the next launch).

**The device does not self-heal on restart — it bricks its own membership,
silently.** The in-memory own-published echo-suppression set is lost on restart:

- web `apps/web/src/chat/client.ts:248` — `private readonly ownPublishedIds =
  new Set<number>();`
- iOS `apps/ios/Lakko/Chat/ChatClient.swift:57` —
  `private var ownPublishedIds: Set<Int64> = []` (the receive-loop drop uses it at
  `:275`).
- Android `apps/android/.../chat/ChatClient.kt:88` —
  `private val ownPublishedIds: MutableSet<Long>`.

The receive loop suppresses a committer's own commit echo via
`markOwnPublishedCommit` (web `chat/client.ts`). ~~The client doc states plainly
that "Without that suppression `processIncoming` raises 'cannot process own
commit'" (`chat/client.ts:1511-1512`).~~ **That quotation was fabricated and is
withdrawn** — the string appears nowhere in the tree, and the cited lines are past
the end of a 1225-line file. The real supporting comment sits beside the
echo-matching loop in `apps/web/src/chat/client.ts` and says the same thing about
the same mechanism: deleting an echo id on match rather than on cursor advance
would mean "the batch gets re-fetched, our own row would then be handed to OpenMLS,
and every subsequent poll would log a `CannotDecryptOwnMessage` for it." The
argument is unchanged; only the citation was wrong. That set is **in-memory only**,
so after a restart it is empty. The poll loop then re-fetches C and feeds it to OpenMLS, which
**unconditionally rejects processing one's own commit at its own epoch**: the
device's `own_leaf_index()` equals the commit's sender leaf →
`ValidationError::CannotDecryptOwnMessage`
(`openmls-0.8.1/src/framing/validation.rs:111-112`). The device is stuck at epoch N
for that group: it cannot advance via C (own-commit reject), and its own future
messages at N are rejected by peers who are at N+1. **Bricked membership, silent,
no error surfaced to the user, recoverable only by another admin re-adding the
device.** Blast radius: one device — the committer.

There is also a **residual 2^-32 handshake-ratchet nonce-reuse surface**: if the
bricked device, still at epoch N, issues a **second** commit at N (a user retry),
it re-consumes the epoch-N handshake ratchet. MLS bounds accidental handshake nonce
collision to ~2^-32 via the 4-byte `reuse_guard` XORed into the nonce (RFC 9420
§6.3.2), so this is a low-probability, secondary surface. The **dominant** hazard
is the deterministic desync brick above.

## The load-bearing design decision

### Why the naive fix is WRONG (state this first)

The G-34 T2 fix was a simple **persist-before-publish reorder** because a skipped
application-ratchet generation is **harmless** — receivers tolerate sender-ratchet
gaps, so aborting a send after the ratchet advanced (leaving generation N unused)
costs nothing.

**A commit cannot be skipped.** N → N+1 is a **group-wide barrier**: every member
must apply exactly the same sequence of commits, in order, to stay in the same
epoch. A bare persist-before-publish reorder here does not fix the problem — it
**flips the desync to the opposite side**:

- Reordered to `create (eager-merge → N+1) → persist(N+1) → publish`, a crash
  between persist and publish leaves the device **durably at N+1** while C **never
  reached the DS**. C's bytes are not recoverable from the merged snapshot (the
  create-commit output was in memory and is gone). So **peers are stuck at N with
  no path forward** (they never see C, and this device — now at N+1 — cannot
  re-emit it), while this device is ahead. The **whole group** is stranded,
  arguably **worse** than the single-device brick the current ordering produces.

So the correct fix is **not an ordering change** — it needs **durability of the
pending commit itself**: the commit (and its Welcome) must survive the crash so it
can be **re-published** to catch the group up.

**Precedent + contrast — the DeviceGroup pairing path.** G-5 / G-7
(`docs/security/known-gaps.md:332,465`) already reordered the DeviceGroup
**pairing** commit to persist-before-publish, and it is safe there — but for a
reason that does **not** carry to chat membership commits. A DeviceGroup add is a
founder adding a *new* device; a crash after persist leaves the founder "ahead"
with an inert added leaf, and "the joiner simply never receives the Welcome and
retries the QR with a fresh KeyPackage" (G-7) — the ahead-state **self-heals via
joiner-retry**, and for the founder + first-pair case "there is no other member,
so the row is a harmless no-op" (G-5). A chat membership commit has **no
self-healing retry** (a remove / role change has no joiner; an add's existing
members must see C to advance) and has **real divergent members**, so the bare
reorder that is safe for pairing **bricks the whole group** here. That is exactly
why chat commits need the durable outbox, not the bare reorder.

### The decision framing the maintainer must accept

Keep the core's eager merge unchanged (no `lakko-mls` touch — invariant #3 stays
untouched). Add a **per-client, per-group, fail-closed durable outbox** that holds
the pending commit's wire-bytes (and its Welcome/rendezvous wire-bytes) alongside
the N+1 snapshot, persisted **atomically before any publish**, and re-published
idempotently on restart. The load-bearing property is:

> **The device is never durably ahead-without-the-commit-published, and never
> durably behind-with-the-commit-published.** The outbox entry and the N+1 snapshot
> are committed to durable storage **together, before the publish**, so a durable
> N+1 view **always** implies the commit is recorded and will be (re)published, and
> a device still at N **always** implies no commit was published.

## Decision

Add a durable **pending-commit outbox** to the three chat clients. **No
`lakko-mls` core change, no FFI change** — the fix is entirely client-side
orchestration over the existing (audited) create-commit / publish / persist
primitives.

### 1. The outbox entry

A per-`(account, group)` durable record, in the same device-backed store the
snapshot uses (web IndexedDB, iOS Keychain/file, Android encrypted store), holding:

- `group_id`
- `target_epoch` (N+1)
- the **commit wire-bytes** (C, exactly the bytes the create-commit call
  produced),
- the **Welcome/rendezvous wire-bytes** for add / approve (the `WelcomeRendezvous`
  envelope per ADR-0027), or none for remove / role,
- an `acked` flag (initially **false**),
- the published DS `messageId`(s) once known (for echo-filter re-arming — §5).

The entry is small and holds only **MLS commit ciphertext + a HPKE-sealed Welcome**
— opaque public artefacts, no new plaintext or PII (invariants #1 / #2 hold; the DS
already stores these bytes opaque).

### 2. The reordered flow (per commit path)

1. **create-commit** — the existing core call eagerly merges N → N+1 in memory and
   returns C (+ any Welcome).
2. **Durably persist, FAIL-CLOSED, BEFORE any publish**, in one atomic write:
   (a) the outbox entry (C + Welcome bytes, `acked = false`), **and** (b) the N+1
   MLS snapshot. If the atomic write fails → **ABORT**: do **not** publish; surface
   a failed op to the user. (On the next restart the device is at N with no outbox
   entry — clean, as if the op never happened; the user retries.)
3. **publish** C (+ the Welcome rendezvous) to the DS. Record the returned
   `messageId`(s) into the outbox entry and `markOwnPublishedCommit(messageId)` (the
   in-memory echo filter, same as today).
4. **mark the entry `acked = true` + persist** (best-effort — a crash here just
   causes a harmless idempotent re-publish on restart).
5. **On restart / group-open** — sweep the outbox (§5).

**Atomicity note (implementation, flagged).** Step 2 relies on committing the
outbox entry and the snapshot **together**. Where the platform store supports a
cross-store atomic transaction (IndexedDB `transaction([...], "readwrite")`, a
single combined blob write, a SQLite transaction) this is direct. Where it does
not, the ordering must be **outbox-entry-first, then snapshot**, and the restart
sweep must reconcile the one torn middle state — an outbox entry present but the
snapshot still at N (the snapshot write failed after the entry write). Because the
publish (step 3) is **strictly after** step 2 completes, that torn state provably
means C was **never published**, so the sweep **discards the entry and stays at N**
(clean). The forbidden state "snapshot at N+1 with no outbox entry" is impossible
by construction under both the atomic and the entry-first ordering. This is the one
genuinely under-specified point that S2 must nail per platform (§ Open questions).

### 3. Two-artifact coverage

The add and approve paths publish **two** artifacts — the commit **then** the
Welcome/rendezvous. The outbox entry MUST hold **both**, so a crash between the two
publishes re-publishes **both** on restart. Otherwise a re-published commit with no
re-published Welcome would advance existing members to N+1 while leaving the new
member stranded with no Welcome to join from. Remove and role paths have no Welcome
— the entry holds only C.

### 4. Crash-window analysis (the correctness argument)

Every crash point maps to a safe outcome. Let "durable N+1" mean the step-2 atomic
write landed.

| Crash point | Durable state | Restart outcome |
|---|---|---|
| **Before step 2 completes** (create done, atomic persist not committed) | snapshot **N**, no outbox entry | Device at N, no pending op → **clean** (op never happened; C never published; peers still at N). |
| **Step 2 atomic write fails** (explicit abort) | snapshot **N**, no outbox entry | Same as above — **clean**. No publish was attempted. |
| **After step 2, before publish** (durable N+1, un-acked entry) | snapshot **N+1**, entry un-acked | Sweep **re-publishes** C (+ Welcome) idempotently → peers reach N+1 → **consistent**. C had not published pre-crash, so no duplicate. |
| **After publish, before ack** (durable N+1, un-acked entry, C on DS) | snapshot **N+1**, entry un-acked | Sweep **re-publishes** C idempotently → the duplicate is dropped by peers (already at N+1 → wrong-epoch stale) and by the committer (echo filter re-armed, §5) → **consistent**. |
| **After ack** | snapshot **N+1**, entry acked | Sweep skips the acked entry → **consistent** (steady state). |

The two dangerous durable states never occur: **"N+1 without a recorded commit"**
(the bare-reorder failure — the whole group stranded) is impossible because the
outbox entry is committed **with** the N+1 snapshot; **"N with the commit
published"** (today's single-device brick) is impossible because the publish is
strictly after the durable N+1 write. Every reachable durable state is either
"clean N, no op" or "N+1 with a re-publishable commit."

### 5. Idempotent restart re-publish + echo-filter re-arm

On restart / group-open, for each **un-acked** outbox entry:

- **Re-publish** C (+ the Welcome rendezvous). The DS journal is append-only, so
  this appends a **duplicate** row. Idempotency at the two consumers:
  - **Peers** already at N+1 receive the duplicate C as an **epoch-N (past-epoch)
    commit** and drop it via the poll loop's existing wrong-epoch tolerance (a
    commit whose epoch < current is stale). A re-posted Welcome rendezvous is
    matched-once by the joiner (idempotent join) and ignored by existing members
    (not an MLS message — their receive loop already drops rendezvous rows,
    ADR-0027).
  - **The committer itself**, durably at N+1, must not feed its own re-fetched C
    into `processIncoming` (that raises `CannotDecryptOwnMessage`). The sweep
    therefore **re-arms `markOwnPublishedCommit`** for the newly-returned
    `messageId` before the poll loop runs.
- **Ack + persist** the entry (best-effort).

**The orphan-duplicate edge (flagged, needs an S2 decision).** If the crash landed
**after** the original publish but **before** the original `messageId` was durably
recorded in the entry (step 3's record is itself a write with a window), the sweep
re-publishes and re-arms the **new** `messageId`, but the **original** duplicate row
carries an *unrecorded* `messageId` the sweep cannot re-arm — so the committer's
poll loop re-fetches it and hits `CannotDecryptOwnMessage`. The robust companion is
to make the receive loop treat `CannotDecryptOwnMessage` at a **past-or-current
epoch the device has already merged** as a **benign drop-and-continue**, not a fatal
error — safe precisely because the outbox guarantees the device is durably at N+1
before it ever sees C, so any own-C re-fetch is already-applied. Note this is a
**companion**, not a substitute: benign-drop **alone** (without the outbox) would
merely convert today's loud brick into a silent stuck-at-N, since the device still
could not reach N+1. Whether S2 records the `messageId` inside the step-2 atomic
write (eliminating the window) or relies on the benign-drop companion is the second
under-specified point (§ Open questions).

## Alternatives

- **Option B — defer the eager merge until publish is confirmed.** Split
  create-from-merge in the core (`add_member` / `remove_member` /
  `set_group_context_extensions` / `approve_external_join` return the staged
  commit **without** merging; the caller merges only after a durable, confirmed
  publish) + thread it through the FFI + all three clients. **Cleaner in principle**
  — the device stays at N (matching the group) until C is durably published, so
  there is no ahead/behind window at all. **Rejected for v1** because it touches the
  **sacrosanct MLS core** (`lakko-mls`, invariant #3 — ADR + property tests + audit)
  and both FFI surfaces, a far larger and riskier change than the client-only
  outbox, for the same end-state guarantee. Recorded as a **possible future
  refinement**: if a later phase reworks the commit FFI anyway, folding in
  defer-merge would remove the outbox's re-publish machinery. Option A ships the
  fix now with **zero core / FFI risk**.

- **Bare persist-before-publish reorder** (the G-34 T2 shape, applied verbatim to
  commits). **Rejected** — it flips the desync from device-behind (single-device
  brick) to **device-ahead** (durable N+1, C never on the DS, C's bytes
  unrecoverable), which strands the **whole group** with no path forward. Strictly
  **worse** than the current bug. This is the load-bearing "why not the obvious fix"
  result: the commit's *durability*, not its *ordering*, is what must change.

- **Do nothing.** **Rejected** — the failure is a **deterministic**, silent,
  single-device membership brick on any window hit (crash or a swallowed persist
  failure), plus the 2^-32 handshake residual. It degrades every group the device
  administers.

## Consequences

- **Positive.** The four commit paths become crash-safe on all three clients with
  **no `lakko-mls` core change and no FFI change** (invariant #3 untouched — the
  audited MLS state machine is not modified). The device is never durably
  ahead-without-publish nor behind-with-publish; a crash anywhere in the window
  self-heals via the idempotent restart re-publish. The two-artifact coverage keeps
  add/approve joiners from being stranded. The 2^-32 handshake residual is removed
  along with the deterministic brick (the device no longer rolls back to N to
  re-commit).

- **Negative / new risks.** A new durable per-`(account, group)` outbox store on
  each client (small — one entry per in-flight commit; cleared on ack). The step-2
  atomicity + the orphan-`messageId` reconciliation are real implementation
  subtleties that S2 must get right per platform (§ Open questions) — an incorrect
  atomicity or a missed echo-filter re-arm re-opens a narrower brick. The idempotent
  re-publish appends duplicate DS rows (bounded — one per crashed-mid-window commit,
  dropped by peers as wrong-epoch); it relies on the poll loop's existing
  wrong-epoch tolerance, which S2 must confirm on each client.

- **Neutral.** Additive and per-client — no wire-format change, no schema/proto
  change, no server change (the DS already stores commit + Welcome bytes opaque).
  The steady-state (no crash) path is byte-for-byte the current behaviour plus one
  extra durable write before publish and one ack write after.

### Invariants touched

- **#1 (no plaintext PII on the server) — holds.** The outbox is client-local; it
  stores commit ciphertext + a HPKE-sealed Welcome — no PII, and nothing leaves the
  device that does not leave it today.
- **#2 (server sees only ciphertext + public artefacts) — holds.** The re-published
  commit + Welcome are the same opaque artefacts the DS already relays; the outbox
  never sends the server anything new.
- **#3 (the MLS state machine is sacrosanct) — untouched.** Option A adds **no**
  `lakko-mls` change — it orchestrates the existing create / publish / persist
  primitives. (This is the decisive reason Option A was chosen over Option B.)
- **#4 (no silent downgrade / fail-closed) — strengthened.** A group-epoch advance
  is never **published** without the commit first being **durably recorded** for
  re-publish; a failed durable write **aborts** the op (no publish) rather than
  proceeding. There is no confidentiality break in the current bug — it is an
  availability / correctness desync (plus the 2^-32 handshake residual) — so this
  ADR does not touch the E2EE guarantee; it hardens durability around it.
- **Invariant-index touchpoints:** this lands as a **free-form G-35 entry** in
  `docs/security/known-gaps.md` — it adds **no** new indexed C / I / T invariant
  (the audit index gains nothing from this ADR; it stood at **8 I + 58 C + 15 T**
  when this was written and is **8 I + 69 C + 15 T** as of 2026-09-02 — the
  substantive claim, that this ADR takes no indexed number, is unchanged), mirroring how the G-33 / G-34
  AEAD-audit records were kept free-form. The `docs/security/invariant-index.md`
  cheat sheet is unchanged.

**Flag this ADR + the S2–S4 implementation through `lakko-security` before merge**
(maintainer-invoked, per the crypto/MLS gate) — it touches the MLS commit
publish/persist ordering, the crash-recovery reconciliation, and invariant #4,
even though it makes no `lakko-mls` core change. The review should confirm the
step-2 atomicity holds per platform, the forbidden "N+1 without a recorded commit"
state is unreachable, and the echo-filter re-arm (plus the benign-drop companion)
closes the orphan-`messageId` edge.

## Open questions

- **Step-2 atomicity per platform.** Whether each client commits the outbox entry +
  the N+1 snapshot in a single atomic store transaction, or as entry-first-then-
  snapshot with the restart-sweep discard-on-torn-middle reconciliation (§ Decision
  2). Both are correct; the choice is per-platform store capability. **S2 decides.**
- **Orphan-`messageId` handling.** Whether to record the published `messageId`
  inside the step-2 atomic write (closing the window entirely) or to add the
  `CannotDecryptOwnMessage`-is-benign-drop companion to the receive loop as
  defence-in-depth (§ Decision 5). **S2 decides.**
- **Outbox retention / GC.** An acked entry is cleared immediately; an un-acked
  entry that repeatedly fails to re-publish (a persistently unreachable DS) needs a
  retry/backoff + a user-surfaced "membership change pending" state rather than an
  unbounded silent retry. Deferred — a UX + retry-policy follow-up.
- **~~Option B as a later refinement.~~ PARTLY ANSWERED by
  [ADR-0045](0045-remote-commit-epoch-fork.md) (2026-08).** Defer-merge landed in
  the core, but **narrowly**: only the new proposal-sweep path
  (`Group::stage_pending_proposals` -> `merge_staged_sweep` /
  `abandon_staged_sweep`) defers. The direct-commit paths this ADR covers
  still merge eagerly and are byte-for-byte unchanged, so **Option A remains the
  v1 mechanism for them** — though as of ADR-0045 S7 (`576ea14`) there are three
  rather than four, and none of the three is reachable from a role-table group.
  Two corrections to the expectation recorded above:
  the invariant-#3 cost was **paid** (under ADR-0045, with property tests; the
  external crypto review is tracked once in
  [G-39](../security/known-gaps.md) per
  [ADR-0046](0046-external-crypto-review.md), as a release gate rather than a
  per-ADR blocker), and it does **not** remove the
  re-publish machinery — the outbox answers *did my commit reach the wire* while
  the staged pair answers *did it win the wire order*, and the first must be
  *yes* before the second is meaningful. The two compose; neither subsumes the
  other. Still open: whether the four direct paths ever follow.
- **DeviceGroup + channel PublisherGroup parity.** The same publish-before-persist
  shape exists on the DeviceGroup pairing path (already reordered, G-5/G-7, and
  safe there for the self-healing-joiner reason above) and would apply to the
  channel co-publisher external-join approve (ADR-0028 OQ3, a `PublisherGroup`
  commit). Whether the outbox generalises to those is out of scope here (this ADR is
  the four chat-group membership paths) — noted for a follow-up if the same window
  is found live there.

## Sprint decomposition

- **S1 — this ADR + the G-35 known-gaps entry.** (This deliverable.) The decision,
  the crash-window correctness argument, the naive-fix-is-worse result, and the
  G-35 record. No code.
- **S2 — web reference implementation.** The durable outbox store
  (`apps/web/src/chat/`), the reordered flow on the four `contacts/client.ts`
  commit paths (`addMember` / `removeMember` / `setMemberAdmin` /
  `approveExternalJoin`), the two-artifact coverage (commit + `WelcomeRendezvous`
  for add/approve), and the restart re-publish sweep + echo-filter re-arm. Resolve
  the two open questions (step-2 atomicity via IndexedDB transaction; orphan
  handling). Vitest coverage for each crash point in the § Decision 4 table.
- **S3 — iOS mirror.** The same shape over `ChatRepository.swift` /
  `ChatClient.swift` (`persistSnapshotBestEffort` seam → durable outbox), verified
  `xcodebuild`.
- **S4 — Android mirror.** The Kotlin twin over `ChatRepository.kt` /
  `ChatClient.kt`, verified `:app:compileDebugKotlin` + `:app:testDebugUnitTest`.
- **S5 — docs reconcile.** Flip this ADR's Status to fully landed; mark **G-35**
  CLOSED in `docs/security/known-gaps.md` (with the touchpoints + the residual, if
  any); add the CLAUDE.md ADR-table row + the module-map note (the three chat
  clients gain a pending-commit outbox on the four commit paths). No
  invariant-index change (free-form G-entry; this ADR takes no indexed number —
  the audit index stood at 8 I + 58 C + 15 T when this was written and is
  8 I + 69 C + 15 T as of 2026-09-02).

## References

- **G-34** (`docs/security/known-gaps.md:2754`) — the system-wide AEAD
  `(key, nonce)`-reuse audit; its **T2** fix (commit `ca318f3`) closed the
  chat-**send** publish-before-persist ordering and explicitly named the commit-path
  residual (`:2823-2825`) this ADR closes.
- **G-5 / G-7** (`docs/security/known-gaps.md:332,465`) — the DeviceGroup pairing
  persist-before-publish precedent, safe there for the self-healing-joiner reason
  that does **not** carry to chat membership commits (the contrast in § The
  load-bearing design decision).
- **ADR-0022** — group-admin roles; the role-change commit path
  (`set_group_roles` → `set_group_context_extensions`).
- **ADR-0027** — external-join approval; the approve commit path
  (`approve_external_join` → `add_member`) and the `WelcomeRendezvous` transport the
  two-artifact coverage re-publishes.
- **ADR-0028** — group join-ticket; the channel co-publisher external-join approve
  (a `PublisherGroup` commit — the noted follow-up parity case).
- **RFC 9420** (MLS) §12 (commit / epoch barrier semantics — a commit cannot be
  skipped), §6.3.2 (the `reuse_guard` bounding accidental handshake nonce collision
  to ~2^-32).
- `openmls-0.8.1/src/framing/validation.rs:111-112` — the
  `CannotDecryptOwnMessage` reject that bricks a device stuck at its own commit's
  epoch.
