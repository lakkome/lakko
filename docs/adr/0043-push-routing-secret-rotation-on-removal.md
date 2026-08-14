<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0043: Rotate the push routing secret on member removal + make per-group unsubscribe live (G-36 residual 2)

- **Status:** **Accepted** — implemented end-to-end on web + iOS + Android
  (`935250a` web reference, `a6ed544` the two native mirrors). The maintainer
  **ratified option (b)**: the owner **or an admin** may rotate. See
  § Outcome — what landed for the per-sprint result and for the corrections to
  this ADR's own prose that implementation exposed.
  This ADR **extends** [ADR-0042](0042-push-opaque-group-routing-id.md)
  — it does not supersede it: the `push_gid` derivation, the 62-byte in-group
  distribution envelope, the **owner-mints rule** (rotation never mints, so
  minting stays owner-only), the defer-never-downgrade posture, and the untouched
  `services/push-gw` all carry through verbatim. The authority-gated receive path
  carries through in **shape but not verbatim** — ratified option (b) widened its
  admin row from a refusal to an acceptance (§ Outcome). It revisits exactly one
  property ADR-0042 ratified —
  "the secret does not rotate on member removal in V1" — and realises the
  rotation-on-removal enhancement ADR-0042 explicitly parked.
- **Date:** 2026-08-10 (proposed) / 2026-08-11 (accepted — implemented on all
  three clients; client-only, `git diff 5665018..a6ed544 -- crates/ services/
  schemas/` is **empty** — the endpoint is pinned to this ADR's own landing
  commit, since a HEAD-relative form decays: ADR-0044 later changed
  `crates/lakko-contacts` for the v2 envelope)
- **Context:** Phase 3 — the push subsystem. Addresses **G-36 residual 2** (a
  removed member keeps a valid `push_gid`, compounded by per-group unsubscribe
  being dead code on all three clients).

## Context

ADR-0042 closed the push social-graph leak by routing every push key-space site
under `push_gid = keyed-BLAKE3(push_routing_secret, "lakko/push/group-routing-id/v1")`
over 32 CSPRNG bytes minted once by the [ADR-0022](0022-group-admin-roles.md)
owner and distributed in-group over an E2EE MLS application message. It
deliberately made that id **stable**, and it ratified the no-rotation choice in
three places — this ADR revisits a decision the maintainer was asked to accept
explicitly, which is why it is a new ADR rather than an in-place amendment:

> **Rotation policy (V1).** Distribute the secret at group creation + to each new
> joiner; **do NOT rotate on member removal in V1.**
> (`0042-push-opaque-group-routing-id.md:206-213`)

> **The maintainer must accept the framing** — "V1 introduces a new stable
> per-group secret [...] it does not rotate per epoch, and it does not rotate on
> member removal in V1" — before implementation.
> (`0042-push-opaque-group-routing-id.md:130-133`)

> **Rotation-on-removal (forward unlinkability) — DEFERRED, unchanged.** V1 does
> not rotate on removal, so a removed member keeps a valid `push_gid`. Compounded
> in practice because per-group unsubscribe is never called on any client [...]
> Tracked under G-36. (`0042-push-opaque-group-routing-id.md:329-332`)

Two coupled residuals motivate reopening it.

### Residual A — an ex-member keeps a working routing id (the primary motivation)

A removed member still holds the group's `push_routing_secret` on disk, so it can
still derive the current `push_gid`, and — because nothing ever tears its
subscription down — **push-gw keeps waking that device for a group it has been
evicted from**. The consequence to state plainly is not storage hygiene: it is
that **an ex-member learns "this group was active at time T" after eviction**, on
every send, indefinitely. That is a live post-eviction metadata channel, not an
accumulation of dead rows.

(The weaker framing — "orphan rows accumulate" — is a real second-order effect and
is addressed by the same change, but it is not the reason to build it.)

### Residual B — per-group unsubscribe is dead code on every client

The wrapper exists on all three clients and has **zero call sites** — verified by
a repo-wide grep, which returns only the three declarations themselves:

| Client | Wrapper | Callers |
|---|---|---|
| Web | `apps/web/src/push/client.ts:204` (`unsubscribeGroup`) | none |
| iOS | `apps/ios/Lakko/Push/PushClient.swift:165` (`unsubscribeGroup`) | none |
| Android | `apps/android/app/src/main/kotlin/me/lakko/app/push/PushClient.kt:197` (`unsubscribeGroup`) | none |

The **server side is fully implemented and live**: `POST /push/group/unsubscribe`
is routed at `services/push-gw/src/main.rs:250` and handled by
`registry.rs:275-300` — a PoW-gated, idempotent
`DELETE FROM push_group_subscriptions WHERE device_id = ? AND group_id = ?`
(`registry.rs:284-288`) returning `removed: bool`. Nothing needs to be built on
the server; the clients simply never call it.

### Why the two are coupled — the sharp reason

They are not two independent fixes that happen to land together. **Rotation
without unsubscribe is actively worse than doing nothing to the store.**

`push_group_subscriptions` is keyed `(device_id, group_id)` and the routing id is
the group key. Rotate the secret and every remaining member subscribes under the
**new** gid — while its row under the **old** gid stays. After k removals push-gw
holds `gid_1: {A,B,C,D}`, `gid_2: {A,B,C}`, `gid_3: {A,B}` … Each rotation
converts a live co-membership cluster into a live cluster **plus a frozen
snapshot of the previous one**: one dead row per remaining device per rotation.
That is a strictly richer artifact for the exact adversary G-36 residual 1
describes — the cluster is now versioned, and the diff between consecutive
versions is legible.

So rotation **requires** unsubscribe to not regress the store, and unsubscribe
alone does not help while the ex-member can still derive the id. Neither half is
shippable without the other.

### What exists today (recon, re-verified)

- **The removal commit path is already a clean seam.** Web
  `ContactsClient.removeMemberFromGroup` (`apps/web/src/contacts/client.ts:1849`)
  creates the Remove commit, persists an [ADR-0040](0040-durable-pending-commit-outbox.md)
  outbox entry + the N+1 snapshot atomically, then publishes; the UI caller is
  `onRemoveMember` (`apps/web/src/routes/Chat.tsx:2191`). The native twins are
  `ChatRepository.removeMemberFromGroup` (iOS `:841`, Android `:750`).
- **The redistribute-after-add precedent is exactly the shape to mirror.**
  `redistributeSecretToGroup` (`apps/web/src/push/group-routing.ts:577`) already
  fires after any add-member commit; iOS `PushGroupRouting.redistribute` (`:474`)
  and Android `GroupPushRouting.redistribute` (`:387`) are its twins.
- **The secret store overwrites in place; the route store does not.** Web
  `putGroupRouting` (`group-routing-store.ts:172`) writes both rows in one
  transaction, but the `secrets` store is keyed
  `accountId + separator + groupIdB64` (overwritten on rotation) while the
  `routes` store is keyed **`pushGidHex`** — so a rotation **adds** a row under
  the new gid and leaves the old one behind. The same asymmetry holds on iOS
  (`PushRoutingStore.putRouting:91`, `routeKey(pushGidHex)`) and Android
  (`PushRouteStore.put:63`).
- **No per-gid delete exists on any of the three route stores.** The full
  surfaces are put / lookup / clear:
  web `putGroupRouting` · `getGroupRoutingSecret` · `hasGroupRoutingSecret` ·
  `lookupPushRoute` · `clearAllGroupRouting` (a *global* wipe);
  iOS `putRouting` · `getSecret` · `hasSecret` · `lookupRoute` ·
  `clear(accountIdB64:)` (a *per-account* wipe);
  Android `PushRouteStore.put` · `lookup` · `clearForAccount`.
  Deleting one stale reverse-index entry is **new work on all three**.
- **No client has any "you were removed" handling.** A repo-wide grep for such a
  path across `apps/web/src`, `apps/ios/Lakko`, and
  `apps/android/app/src/main` returns nothing. `removeMemberFromGroup`'s own
  doc comment (`contacts/client.ts:1830-1832`) says the removed member's client
  "surfaces as a generic 'you're no longer in this group' signal at the
  application layer" — that layer does not exist. **An ex-member's device will
  never clean up after itself**, which is what makes the property this ADR buys
  (§ crux) load-bearing rather than cosmetic.
- **`ensure` will not re-broadcast once a secret is held.**
  `ensureGroupPushRouting` (`group-routing.ts:509`) mints + broadcasts only under
  `isOwner && !(await hasGroupRoutingSecret(...))`. Since the secret is persisted
  **before** the broadcast, a failed broadcast leaves the guard satisfied and the
  next launch short-circuits. (The in-code comment claiming the broadcast
  "retries on the next launch / add-member" is accurate only for the add-member
  half — `redistributeSecretToGroup`; the launch half does not happen. Noted here
  because § Consequences depends on it.)

## The load-bearing design decision

State precisely what rotation buys and what it does not, before the design.

### What it buys

**Forward-unlinkability of the routing _id_, unilaterally.** After a rotation
nobody ever notifies the old gid again, so the ex-member's push-gw row goes
**inert without the ex-member's cooperation**. That last clause is the whole
point: `POST /push/group/unsubscribe` deletes exactly the `(device, group)` row
named in the request (`registry.rs:284-288`), and a client holds only **its own**
device pseudonym — so no remaining member can tear down the ex-member's row, and
the ex-member's own client has no removal handling to do it itself (§ Context).
Rotation is the only mechanism available to the group that ends the ex-member's
wake stream. It also ends the growing archive described above.

### What it does NOT buy

**Unlinkability of the _cluster_.** The provider observes `{A,B,C,D}` under
`gid_1`, then `{A,B,C}` under `gid_2`. Identical-or-near-identical device-token
sets across a rotation boundary **re-link by intersection** — a rotation is not a
mixnet — and the set difference reveals **which device was removed**. That is
arguably a signal the stable id did not hand over so legibly: today a removal is
invisible to the provider (nothing changes); after this ADR a removal is a visible
re-keying event with an identifiable diff.

This is a real, deliberate trade: the ex-member's ongoing wake stream (a channel
to a party who has been evicted and is therefore adversarial by assumption) is
closed at the cost of making removals legible to the provider (a party who already
sees the cluster and its churn continuously). We judge the first strictly worse
than the second, but it is a judgement and must be recorded as one.

Closing the cluster leak needs **per-device-blinded routing ids** — each
subscriber registering under an id only it and the publisher can compute, so no
two devices share a key-space value — plus payload minimisation. That is **out of
scope here** and remains **G-36 residual 1**.

### This is not a reversal of ADR-0042's Option-A rejection

ADR-0042 rejected Option A (an epoch-rotating exporter-derived id) on churn
grounds (`0042:217-228`): a group-wide re-subscribe on **every** membership
commit, a **moving** reverse-map window in the NSE / service worker, and an APNs
`thread-id` that changes every epoch, fracturing lock-screen grouping.

**Every one of those is a cost of _per-epoch_ rotation, not of rotation.**
Rotate-on-removal pays them **once per removal** — a rare, deliberate,
user-initiated event — and keeps the id perfectly stable in the common case
(sends, adds, joins, role changes, every ordinary commit). The reverse-map stays
a fixed per-group entry between removals rather than a moving window; the
`thread-id` changes at most once per removal, which fractures lock-screen grouping
exactly at the moment the group's membership visibly changed.

Rotate-on-removal is a **third point on the axis that ADR-0042 never priced** —
it evaluated "stable forever" against "rotates every epoch" and correctly picked
the former. The intermediate point costs the churn only at removals.

## Decision

**Re-mint and redistribute the `push_routing_secret` when a member is REMOVED
from a group, and make per-group unsubscribe live so the old routing id is torn
down as the new one is armed.**

### Rotate on removal only, never on add

An added member is *supposed* to route — it needs the current secret, which is
precisely what `redistributeSecretToGroup` already delivers. Rotating on add would
additionally **break the ADR-0042 admin-seed delivery path**: the `accept-identical`
and `accept-admin-seed` verdicts (`group-routing.ts:271-296`) exist so that an
admin-performed add actually delivers routing to the new member. A rotation on add
turns that redistribute into a *changed* secret from an admin against a *stored*
secret on every existing member — `reject-admin-overwrite` — so the group would
split. Zero privacy gain, guaranteed breakage. Removal only.

> **CORRECTION (post-ratification of option (b)).** The *conclusion* — rotate on
> removal only — stands, but **half this paragraph's reasoning no longer holds.**
> Under (b) an admin's *changed* secret against a *stored* one is
> `accept-admin-rotate`, not the (now non-existent) `reject-admin-overwrite`, so a
> rotation on add would **not** split the group. What survives is the other half,
> which was always the stronger argument: **zero privacy gain** (an added member
> is entitled to route) plus pointless churn — a re-key, a group-wide
> re-subscribe, and a `thread-id` change on an event that reveals nothing. The
> code matches: the rotation trigger fires only on a membership **shrink** and
> returns `skip-no-shrink` on growth. The verdict-matrix anchor
> `group-routing.ts:271-296` also drifted — the current matrix table is
> `apps/web/src/push/group-routing.ts:444-452`, inside the doc-comment at
> `:437-466`, whose closing paragraph now records the (b) widening in the code
> itself.

### The per-client flow

On the removing client, after the Remove commit is published:

> **CORRECTION — the flow as written omits its own first exit.** If step 1 finds
> **no** secret (none held, or the row cannot be read), the whole sequence is a
> **no-op**: it returns `skip-no-secret` and mints nothing. **Rotation never
> MINTS.** This is not a detail — it is the property that keeps ratified option
> (b) from creating a second minter: an admin may *replace* a secret the group
> already has, but can never bring one into existence, so mint authority stays
> owner-only exactly as ADR-0042 left it. The rotator is additionally pre-gated
> on being owner-or-admin (`skip-not-authorized`).

1. Read the **OLD** secret for `(account, group)`.
2. Derive the **OLD** `push_gid` from it.
3. Mint a **NEW** secret.
4. **Broadcast** the ADR-0042 distribution envelope carrying the new secret.
5. **Persist** the new secret + the new reverse-index row.
6. **Subscribe** under the NEW gid.
7. **Unsubscribe** this device's row under the OLD gid.
8. **Delete** the OLD reverse-index row.

Every remaining member, on receiving the envelope, runs the ADR-0042 gated
receive path unchanged (`receivePushRoutingSecretGated`), then performs its own
steps 6-8 with the old gid it can still derive from its own about-to-be-replaced
stored secret — which is why step 1 must precede the store write on **every**
client, not just the rotator.

### Ordering is load-bearing twice over

**(a) The rotation broadcast must strictly FOLLOW the Remove commit.** The
envelope is an MLS application message; if it is sent at epoch N (pre-removal),
the evicted member is still a member of that epoch and **can open it**, learning
the new secret and defeating the entire change. Broadcasting at epoch N+1 is safe
because RFC 9420 forward secrecy is exactly the property that denies the removed
leaf the post-Remove epoch's keys. This is a hard sequencing requirement, not an
optimisation, and it must be asserted in a test.

**(b) The OLD secret must be read before the store overwrite.** The secret row is
keyed `(account, group)` and is clobbered in place (web `secretRowKey`, iOS
`secretKey`, Android `rowKey`), and unsubscribe needs the OLD gid. Persist-first
loses the ability to name the row being retired — leaving an undeletable orphan
that keeps receiving wakes, i.e. the exact defect being fixed.

### The genuine fork — who rotates when the owner is offline

> **RATIFIED: (b).** The maintainer chose **(b) — owner OR admin may rotate**, not
> the (a) this section recommends. The rest of this section is preserved as the
> decision record as written; § Outcome — what landed states what was built, and
> in particular the one property that keeps (b) from creating a second minter:
> **rotation never MINTS.** Option (b) widened *overwrite* authority only, so an
> admin may replace a secret the group already has but can never bring one into
> existence — minting stays owner-only, unchanged.

**This is the one decision that needs maintainer ratification**, in the same way
ADR-0042's stability framing did (`0042:130-133`). The tension is structural:
[ADR-0022](0022-group-admin-roles.md) permits an **admin** to commit a removal
("remove a non-owner member -> committer is owner or admin",
`crates/lakko-mls/src/group.rs:571-575`, enforced by `authorise_role_commit` in
`process_incoming_verifying_roles:582`), but the ADR-0042 receive gate permits an
admin only to **seed a provably-absent** secret, never to overwrite one
(`accept-admin-seed` vs `reject-admin-overwrite` — the pre-ADR-0043 gate, whose
decision function was then `group-routing.ts:390-436` at `4e1c8a0`; **that is
exactly the row option (b) widened**, so
`reject-admin-overwrite` no longer exists and the decision core now lives at
`group-routing.ts:468-519`). So
**an admin can evict a member but cannot rotate the routing id**, and the removal
completes with the ex-member's wake stream intact.

**(a) Owner-only, latent — RECOMMENDED for v1.** The owner rotates when it next
observes the membership shrink. Correct, introduces **no new authority**, requires
**no change to the security-reviewed receive gate**, and reuses the existing
owner-mint machinery verbatim. The cost is honest and must be stated: the window
between the eviction and the rotation is "until the owner next opens the app" —
**possibly never**, for a group whose owner has abandoned it. The mitigating
argument is that this is *strictly better than today in every case and never
worse*: today the window is unconditionally infinite; under (a) it is finite
whenever the owner is active. It is a latency bound, not a correctness gap.

**(b) Widen the gate so admins may overwrite.**

> **CORRECTION — the convergence claim below is WRONG, and is replaced, not
> softened.** "The DS journal is a total order and every client processes it in
> order, so last-accepted-wins converges" does **not** follow, because a rotator
> applies **its own** mint at **local** time, not at its own journal position. So
> a device that publishes its rotation at journal id `p` and then polls an earlier
> message `q < p` that it had not yet seen will **overwrite its own newer secret
> with the older one** — and the row is then marked `distributed: true`, so
> nothing ever re-broadcasts to repair it.
>
> Under (b) this is **routine rather than a race**: a single removal produces
> **N+1 rotations** in a group with an owner and N admins — the committer rotates
> directly from its own remove path, and every other owner/admin observes the same
> shrink through its poll diff and rotates too. **Honest statement of the
> property: concurrent rotators can strand a device on a superseded secret, which
> self-heals only at the next rotation.** The affected device keeps deferring or
> routing under a gid nobody notifies; foreground polling still delivers messages
> throughout, so this is a wake-delivery defect, not message loss.
>
> Note the owner-precedence branch is arrival-order-independent *within* the
> receive gate, which is a different and narrower claim than convergence of the
> whole system, and does not rescue the above.

The real cost is also **DoS surface**: it generalises
**G-36 residual 3** from "a malicious admin can seed a secret to a member who
holds none" to "a malicious admin can churn the whole group's routing id at
will", denying every member's wakes repeatedly until the owner rotates back. An
admin can already remove members outright, so the *capability* gain is small — but
this undoes the shape of the ADR-0042 **MEDIUM-1** fix (`21554f2`), which exists
precisely to stop a non-owner from unilaterally rotating the group's routing id.
**It must go past `lakko-security` before merge.** Recorded as the tightening
path if (a)'s latency proves unacceptable in practice.

**(c) Deterministic derivation from the post-removal MLS exporter — REJECTED.**
Superficially elegant: no minter, no broadcast, no authority question at all,
since every member derives the same id from
`export_secret(...)` (`crates/lakko-mls/src/group.rs:785`) at the post-removal
epoch. It fails for a specific reason, and it is not the obvious one. The problem
is not agreement on the *value* — it is agreement on **which epoch's** exporter.
Members advance epochs at different wall-clock times, and a member catching up
later **cannot re-derive a past epoch's exporter secret** (that is forward
secrecy working as designed), so it cannot compute the gid a wake in flight was
routed under, nor reconstruct the reverse-map entry for it. The only self-
consistent rule — "always re-derive at the *current* epoch" — **collapses exactly
back into ADR-0042's rejected Option A** (per-epoch rotation, group-wide
re-subscribe on every commit, a moving reverse-map, an unstable `thread-id`), so
it is not a third option at all.

### The rotation trigger

**Recommended: a client-side `members()` diff after the poll loop applies
commits.** It is free (the member snapshot is already read per poll), touches no
audited core, and needs no ADR of its own. The alternative — surfacing removed
leaves from `lakko-mls` — is **exact** rather than inferred and has a
precisely-shaped precedent in `Group::process_incoming_gated`
(`crates/lakko-mls/src/group.rs:715`), which already reads the removed leaves and
their **pre-merge** signature keys (`:745-759`) and hands them to a refuse-to-apply
gate before the merge (`:761`). Extending the same surfacing to the
role-verifying path is small, but it **is an invariant-#3 change to the MLS state
machine** and therefore needs its own ADR + property tests. Start with the diff;
record the core change as a separate want (§ Open questions), not as a
prerequisite.

**One correction to a plausible shortcut.** The existing `membersVersion` signal
looks like a membership hook — its own comment calls it an epoch-advance signal
("`membersVersion` bumps after the receive loop / add / remove / external-join
applies a commit", `apps/web/src/routes/Chat.tsx:1222-1226`, driving the ADR-0029
mid-call rekey effect at `:1230-1246`) — but it is incremented **unconditionally
after every poll** (`Chat.tsx:1625`), whether or not any commit arrived. It is a
"a poll happened" tick. It is a fine **place to hang** the diff; it is **not** the
signal.

### New store work: a per-gid delete

None of the three route stores can delete a single reverse-index entry (§ Context).
All three need one: web a `deleteRoute(pushGidHex)` over the `routes` object
store, iOS a `removeRoute(pushGidHex:)` over both ordered route defaults
(`orderedRouteDefaults()`), Android a `PushRouteStore.remove(pushGidHex)`. This is
new API on each platform, not a call-site change.

**The overlap question — keep the old route row briefly, or delete it at once —
is NOT a security question.** A route miss already degrades to the generic "New
message" banner under the ADR-0041 fail-closed posture, so retaining the old row
for an overlap window buys exactly **one nicer notification body** for an
in-flight wake, at the cost of a dirtier store and a second deletion pass. Choose
on taste, not on threat. (Recommended: delete immediately — the simpler
invariant, and an in-flight wake at a removal boundary is already a rare event.)

### Durability: a pending-rotation record

> **CORRECTION — NOT BUILT, deliberately, because the premise below went stale
> between this ADR being written and being implemented.** This ADR was authored
> at `5665018`; the three commits `5729aa0` → `9cb8f06` → `7e7d143` landed
> immediately after it and fixed exactly the stranding class this section
> reasons from. The claim below that "`ensure` will **not** self-heal, because
> its `isOwner && !hasSecret` guard is now satisfied" is **no longer true**:
> `ensureGroupPushRouting` gained a third branch keyed on a durable
> `distributed` boolean on the stored secret row, and a secret that is held but
> **not** confirmed on the wire is re-broadcast — **by ANY holder, owner or
> not** (`7e7d143` split the guard by what the branch *does*: mint stays
> owner-only, re-broadcast does not), reached from **both** entry points this
> section asks for (chat-open and the launch sweep). A failed rotation broadcast
> therefore already leaves the row pending and is already retried, without a new
> record. The rotation path reuses that machinery verbatim: it persists the new
> secret with `distributed: false` and flips the flag only after the broadcast
> resolves.
>
> **What the flag does NOT cover, stated precisely.** It recovers the
> **broadcast**, never the **teardown**. Nothing anywhere records the OLD gid
> awaiting retirement — it lives only as an in-memory local between the persist
> and the unsubscribe. Once the `secrets` row is clobbered in place the old bytes
> are unrecoverable, the old `push_gid` can never be named again, and its push-gw
> subscription + reverse-index row become a permanent orphan.
>
> **Two paths reach that orphan, and the likelier one is not a crash.** (1) A
> crash in the window. (2) **A transient secret-store read failure on the receive
> path**: the teardown is conditioned on the previously-stored secret, snapshotted
> before the persist, and a failed read yields `stored = null` on all three
> clients — indistinguishable from "this device held nothing" — so the new secret
> is persisted and the teardown is silently **skipped**. No crash, nothing
> surfaced. That needs only a momentary store hiccup, so it is **more likely** than
> the crash window.
>
> **Verdict: accepted, and the durable record should NOT be built.** The orphan is
> **inert** — nothing ever notifies the old gid again — so it is an accumulation
> defect, not a live channel, and what the stranded row leaks is bounded to "this
> device was once in *some* group", with no linkage to which group or to the
> current cluster. A durable record would cost a persist on a hot path plus a
> launch sweep to reclaim a row that leaks nothing further; that is not
> proportionate. Recorded as a residual in § Consequences and in **G-36**, not
> scheduled. (Were it ever built it would be **lighter** than this section
> prescribes: the new secret is already durable by then, so an entry holding
> `(account, group, new secret, old push_gid)` is over-specified — only the **old
> gid** needs recording.)

Both orderings of steps 4-5 have a bad crash window, and — unlike at mint time —
**both strand a group that was already working**:

- **Persist-then-broadcast**, broadcast fails: the rotator is alone on the new
  gid, every other member is still on the old one and nobody notifies it, so
  **the whole group's wakes stop**. `ensure` will **not** self-heal, because its
  `isOwner && !hasSecret` guard is now satisfied (§ Context) — the group stays
  broken until an add-member happens to fire `redistribute`.
- **Broadcast-then-persist**, persist fails: the rotator is stranded on the old
  secret while everyone else moved — the same break with the victim inverted.

This is materially worse than the equivalent failure at mint time, where nobody
holds a secret yet and every member merely **defers** (no push, but nothing
regresses).

**Therefore: record the pending rotation durably before either side effect, and
retry it on next launch** — borrowing the shape of the ADR-0040 commit outbox
(`apps/web/src/chat/commit-outbox.ts`: `putEntryDurable` before publish,
`markAcked` after, `loadUnacked` + a startup sweep, `deleteEntry` on completion).
A rotation entry holds `(account, group, new secret, old push_gid)` and is acked
once the broadcast lands; the sweep re-broadcasts un-acked entries idempotently
(a re-received identical secret is `accept-identical` on every peer, so the retry
is free of side effects).

### The delivery hole is bounded

Between the rotator's local switch and each member's next poll of the group
journal, a send notifies the **new** gid while that member is still subscribed to
the **old** one: **its wake does not arrive** for at most one poll interval. The
message itself is unaffected — it lands on the next poll like any other. Bounded,
self-healing, no data loss. Worth stating so it is not discovered as a bug.

## Alternatives

- **Do nothing (keep ADR-0042's V1 policy).** Rejected. The ex-member's wake
  stream is unbounded in time and, because no client has removal handling, the
  ex-member's device will never stop it. "The removed member already knows the
  public `group_id`" (the ADR-0042 justification, `0042:210-213`) is true of the
  *identifier* but does not cover the *ongoing activity signal*, which is what a
  live subscription delivers.

- **Unsubscribe only, no rotation.** Rejected — it cannot reach the party that
  matters. `group_unsubscribe` deletes only the `(device, group)` row named in the
  request, and only the ex-member's own client knows its own device pseudonym; that
  client has no removal handling and never will call it unprompted. Every remaining
  member could tidy its own rows, and the evicted device would keep receiving wakes
  under an unchanged gid. It fixes the store and not the leak.

- **Rotation only, no unsubscribe.** Rejected — the archive effect (§ Context):
  one dead row per remaining device per rotation, turning a single live cluster
  into a versioned history of clusters with legible diffs. It hands G-36 residual 1
  a richer artifact than the status quo.

- **Rotate on every membership change (adds too).** Rejected — but on one ground,
  not two. **No privacy gain** (an added member is entitled to route) plus
  pointless churn. The second ground as originally written — that it
  "deterministically breaks the ADR-0042 admin-seed path into
  `reject-admin-overwrite` across the existing membership" — **lapsed with the
  ratification of option (b)**, under which that case is `accept-admin-rotate`
  and nothing breaks (§ Decision, correction note).

- **Per-epoch rotation (ADR-0042 Option A, revived).** Still rejected, on ADR-0042's
  own grounds (`0042:217-228`) — group-wide re-subscribe per commit, a moving
  reverse-map window, an unstable APNs `thread-id`. Rotate-on-removal obtains the
  forward-unlinkability benefit at a fraction of the churn (§ crux).

- **Per-device-blinded routing ids.** The genuinely stronger design: each
  subscriber registers under an id only it and the publisher can compute, so push-gw
  and the providers never see two devices share a key-space value — this closes the
  **cluster** leak that rotation explicitly does not (§ crux). Out of scope here: it
  changes the notify fan-out shape (push-gw would need per-recipient ids rather than
  one JOIN key), which is the first change in this arc that would **not** leave
  `services/push-gw` untouched. A future ADR against G-36 residual 1.

## Consequences

- **Positive.** An evicted member's push wake stream ends — unilaterally, without
  needing that member's device to cooperate, which is the only mechanism available
  given the absence of removal handling anywhere in the clients. The growing
  archive of stale subscriptions is bounded to one dead row per removal (the
  ex-member's, which is inert) instead of one per remaining device per rotation.
  Per-group unsubscribe stops being dead code, and its live server endpoint
  (`registry.rs:275-300`) starts being exercised. The routing id stays stable
  across every ordinary commit, so the reverse-map, the deferral posture, and iOS
  lock-screen grouping are unchanged except at removals. Under the recommended
  option (a) there is **no change to the `lakko-security`-reviewed receive gate**
  and **no new authority** anywhere.

- **Negative / new risks.** A removal becomes a **visible re-keying event at the
  push providers**, and the token-set diff across the boundary reveals which
  device left (§ crux) — a legibility regression accepted in exchange for closing
  the ex-member channel. A new crash window whose bad outcome is a **whole-group
  wake outage** rather than a mere deferral, which is why the durable
  pending-rotation record is part of the decision and not an optimisation. New
  per-gid delete API on three route stores. A bounded one-poll-interval wake hole
  per rotation. Under option (a), an abandoned-owner group never rotates — a
  latency bound with no upper limit, better than today but not a guarantee. If
  option (b) is chosen instead, G-36 residual 3 widens from admin-seed to
  admin-churn and the change **must** clear `lakko-security`.

- **Neutral.** `services/push-gw` stays **byte-identical unchanged** — the same
  opaque `BINARY(32)` key-space, the same already-live unsubscribe handler, no
  migration. No `lakko-mls` change under the recommended trigger. No wire-format
  change: the ADR-0042 62-byte envelope and the `push_gid` derivation are reused
  verbatim; only *when* an envelope is emitted changes. The ADR-0041 reverse-map
  read path (`resolveAccount`) is untouched in shape — it gains and loses entries,
  which it already tolerates (a miss degrades to the generic banner).

- **Residuals that survive this ADR.**
  1. **The co-membership cluster is still materialised** (G-36 residual 1) —
     rotation renames it and makes its transitions legible; it does not unlink it.
     Needs per-device-blinded ids.
  2. **The ex-member's push-gw row is never deleted**, only rendered inert. Nobody
     but that device can delete it, and that device has no removal handling.
  3. **Option (a)'s unbounded rotation latency** for an abandoned-owner group.
  4. **G-36 residual 4 (legacy / no-role-table groups get no push at all)** is
     untouched — they have no owner, so nothing to mint and nothing to rotate.
  5. **The stale web `?wake=` deep-link reader** (G-36 loose end) is unrelated and
     untouched.

### Invariants touched

- **#5 (the social graph is not materialised on the server) — improved, still
  bounded.** The push subsystem stops maintaining a subscription for a party the
  group has evicted, and stops accumulating a versioned archive of past
  co-membership sets. It does **not** stop materialising the current cluster: a
  provider still sees N device tokens under one opaque handle, and can re-link
  across a rotation by token-set intersection (§ crux). This is a strengthening of
  the ADR-0042 position, not a closure of it.
- **#2 (the server sees only ciphertext + public artefacts) — unchanged.** The new
  secret rides the same E2EE MLS application message; push-gw keeps handling an
  opaque `BINARY(32)` and is not modified; the unsubscribe request carries only
  pseudonyms it already accepts.
- **#4 (no fallback to non-E2EE) — unchanged.** A member without the current
  secret still **defers** — it does not subscribe at all rather than falling back
  to the raw `group_id`. The rotation makes deferral *more* frequent for a window;
  it introduces no downgrade path. The receive gate stays fail-closed as landed.
- **#3 (the MLS state machine is sacrosanct) — untouched** under the recommended
  `members()`-diff trigger; no `crates/lakko-mls` edit. It becomes relevant only if
  the removed-leaf surfacing is pursued, which is deliberately scoped to a separate
  ADR (§ Open questions).
- **Invariant-index / gap touchpoints (docs sprint, not this ADR).** On landing:
  **C-60** gains the rotation-on-removal property; **G-36 residual 2** is closed
  and its "compounded by dead unsubscribe" clause retired; residuals 1, 3, and 4
  stay open, with residual 3 **widened** if option (b) is chosen. This ADR does not
  touch `docs/security/` or `CLAUDE.md`.

  > **AS LANDED.** **C-60 was refined, not renumbered** — the routing id is the
  > same property, now additionally forward-unlinkable across a removal (new
  > sub-invariant 6), and its clause (4) was corrected where option (b) inverted
  > it. **One new indexed entry was minted: C-61**, for the
  > broadcast-follows-the-Remove-commit **ordering**, on the grounds that a
  > sequencing property a refactor can silently break deserves its own line the way
  > the call-key overlap rules C-8 / C-9 / C-10 do. Counts move to
  > **8 I + 61 C + 15 T**. In G-36, residual 2 is CLOSED, residual 3 WIDENED, and
  > residuals **5** (orphaned old gid) and **6** (the untested C-61 ordering) are
  > new.

**`lakko-security` gate.** Required before merge **if option (b) is chosen** — it
re-opens the MEDIUM-1 fix's authority model and widens a documented DoS residual.
Under option (a) a pass is optional but advisable: the change alters when E2EE key
material is minted and redistributed, and adds a durable record holding a
pre-broadcast secret.

## Open questions

- **Which fork (a) / (b)?** The one item needing maintainer ratification. This ADR
  recommends **(a) owner-only, latent** and records (b) as the tightening path with
  its `lakko-security` precondition.
- **Surfacing removed leaves from `lakko-mls`.** The exact trigger, versus the
  inferred `members()` diff. Precedent exists (`group.rs:715-762`), but it is an
  invariant-#3 change requiring its own ADR + property tests. Recorded as a want,
  not a prerequisite.
- **Overlap window for the old route row.** Delete immediately (recommended) or
  retain briefly for a nicer in-flight notification body. Not a security question
  (§ Decision).
- **Should the ex-member's own client tear down its subscription?** It cannot today
  (no removal handling anywhere). Building that handling would let a well-behaved
  ex-member delete its own row — cleaner, but it must not be *relied* on, since a
  hostile ex-member simply would not. Rotation is the enforcing mechanism; self-
  cleanup would be hygiene on top. Separate work.
- **Rotation on other membership-shrinking events.** v1 covers admin-initiated
  removal. Member self-leave has no API today (ADR-0022 notes OpenMLS
  `CannotRemoveSelf` makes a self-Remove commit impossible), so there is nothing to
  hook; revisit if a leave path is built.
- **Batching consecutive removals.** Removing three members one after another
  triggers three rotations. A short coalescing window would reduce churn to one.
  Deferred — correctness first, and removals are rare.

## Sprint decomposition

For the recommended shape: option **(a)** owner-only latent rotation, triggered by
a client-side `members()` diff.

- **S1 — store surface: per-gid delete + the pending-rotation record.** Add
  `deleteRoute(pushGidHex)` / `removeRoute(pushGidHex:)` /
  `PushRouteStore.remove(pushGidHex)` to the three route stores, and a durable
  pending-rotation record modelled on `apps/web/src/chat/commit-outbox.ts`
  (`putEntryDurable` / `markAcked` / `loadUnacked` / `deleteEntry`) holding
  `(account, group, new secret, old push_gid)`. Unit tests per platform, including
  a delete-then-lookup miss and a sweep over an un-acked entry.
- **S2 — the pure rotation-decision core.** A host-testable function taking the
  previous and current member snapshots plus the local role and returning
  rotate / no-op, mirroring the pure-decision style of `routingSecretVerdict`
  (`group-routing.ts:329`) / `GroupPushRouting.decideAcceptReceivedSecret`
  (`:199`) / `PushGroupRouting.classifyIncomingSecret` (`:385`). Tests: shrink
  fires; growth does not; a no-change poll does not; a non-owner never rotates
  under option (a).
- **S3 — web reference.** Wire the diff at the post-poll seam
  (`Chat.tsx:1615-1625`) and the direct path after `onRemoveMember`
  (`Chat.tsx:2191`); implement `rotatePushRoutingSecret` beside
  `redistributeSecretToGroup` (`group-routing.ts:577`) executing steps 1-8 with
  the durable record; extend the gated receive path so a member applying a
  *changed* accepted secret also unsubscribes its old gid and deletes its old
  route row. **Assert the broadcast-follows-the-Remove-commit ordering in a test**
  (§ Decision, ordering (a)) and the read-old-before-overwrite ordering (b).
- **S4 — iOS + Android mirrors.** The same shape over
  `PushGroupRouting.redistribute` (iOS `:474`) / `GroupPushRouting.redistribute`
  (Android `:387`) and `ChatRepository.removeMemberFromGroup` (iOS `:841`,
  Android `:750`); add an unsubscribe port to the Android
  `PushRoutingPorts` interface (`GroupPushRouting.kt:410-435`, which today exposes
  `subscribeUnderPushGid` and no counterpart). Port the S2 decision core's tests
  verbatim to both platforms, as ADR-0037's `restoreVerdict` gate was.
- **S5 — startup sweep + verification.** Re-publish un-acked rotation records on
  launch on all three clients; verify per-platform (web `tsc` / `vitest` /
  `vite build`, iOS `xcodebuild` + tests, Android `:app:compileDebugKotlin` +
  `testDebugUnitTest`). Confirm `git diff -- services/` is **empty**, as ADR-0042
  Sprint 5 did.
- **S6 — docs reconcile.** Flip this ADR's status; update **C-60** with the
  rotation property; close **G-36 residual 2** and retire its dead-unsubscribe
  clause; keep residuals 1 / 3 / 4 open (widening 3 if option (b) was taken);
  update the `CLAUDE.md` ADR table + the `lakko-contacts` module-map row. Not part
  of this ADR.

## Outcome — what landed

Implemented client-only on all three clients; `git diff 5665018..a6ed544 --
crates/ services/ schemas/` is **empty**, so the ADR-0042 `push_gid` derivation,
the 62-byte envelope, the fuzz target, and `services/push-gw` were all untouched
by *this* ADR. The endpoint is pinned to this ADR's own landing commit rather
than `HEAD`, because the HEAD-relative form decays as later work lands:
[ADR-0044](0044-push-routing-secret-recency.md) subsequently changed
`crates/lakko-contacts` (the envelope went v2, 70 bytes) and the fuzz target with
it. `services/push-gw` remains byte-identical through both arcs.

**Anchor convention for this section:** every line number below is as of
**`a6ed544`** (the commit that landed the iOS + Android mirrors), verified against
that commit rather than a working tree. Symbol names are the durable reference —
as § Corrections item 5 shows, these files' line numbers drift quickly.

| Sprint | Outcome | Commit |
|---|---|---|
| 1 | **Store surface — per-gid delete landed; the durable pending-rotation record did NOT** (see the correction below). Web `deleteRoute(pushGidHex)` (`group-routing-store.ts:454-463`), iOS `PushRoutingStore.removeRoute(pushGidHex:)` (`:346-351`, iterating `orderedRouteDefaults()` so **both** locations `lookupRoute` reads are cleared — the shared App Group suite and the app-private fallback, else a stale fallback row keeps resolving), Android `PushRouteStore.remove(pushGidHex)` (`:105-107`) behind the new `PushRoutingPorts.deleteRoute` port | `935250a`, `a6ed544` |
| 2 | **The pure rotation-decision core.** `membershipRotationDecision` on all three — web `group-routing.ts:1022-1041`, iOS `PushGroupRouting.swift:1007-1021`, Android `GroupPushRouting.kt:859-876` | `935250a`, `a6ed544` |
| 3 | **Web reference** — the gate widening, both entry points, the rotation executor, the receive-side teardown, the in-flight lock | `935250a` |
| 4 | **iOS + Android mirrors** — the same shape, plus two Android-only items forced by the gate change (below) | `a6ed544` |
| 5 | **Verification** — per the commit records: web `tsc` clean + `vite build` + vitest 476 -> 506; iOS `xcodebuild` BUILD SUCCEEDED (both targets) + `PushGroupRoutingTests` 33 -> 51; Android `:app:compileDebugKotlin` BUILD SUCCESSFUL + push suite 85 -> 107. Each platform's commit records bite-proofs (a guard disabled -> named tests fail -> reverted) | `935250a`, `a6ed544` |
| 6 | **Docs reconcile** — this status flip + the corrections below, **G-36** residuals 2 + 3, **C-60** (refined, not renumbered), the `CLAUDE.md` ADR table | (this change) |

### What the ratified option (b) actually changed

The gate's admin row flipped from a **refusal** to an **acceptance**: an admin may
now **OVERWRITE** a stored secret, not only seed a provable hole. Web renamed the
verdict `reject-admin-overwrite` -> **`accept-admin-rotate`**
(`group-routing.ts:416`, returned `:516`); both natives keep a **binary**
accept/reject and simply flip the row (iOS `SecretVerdict` `:468-473`,
`classifyIncomingSecret` `:562-605`; Android `ReceiveVerdict` `:119`,
`decideAcceptReceivedSecret` `:350-381`).

Everything else about the gate is **unchanged**, and each property is still
pinned in code: identical bytes are accepted from **any** sender *before any
authority is consulted* (web `:494-499`, iOS `:572`, Android `:359-361`); the
**owner is evaluated first**, so an owner rotation wins regardless of envelope
arrival order (web `:508`, iOS `:593`, Android `:370`); a **plain member** is
refused (web `:518`, iOS `:603`, Android `:378`); an **unreadable role table** or
an **unresolvable sender** is refused fail-closed (web `:501`/`:506`, iOS
`:582`/`:589`, Android `:364`/`:367`).

**One point worth stating exactly, because it is easy to invert:** a group with
**no role table** is *accepted* by the receive gate (web `accept-no-owner`
`:505`, iOS `:578`, Android `:365`) — that is the legacy/open-group behaviour
ADR-0042 established and this ADR did not touch. It is the **rotation** decision
that refuses such a group (`skip-not-authorized`), because a group with no owner
never had a minter. Receive-accept and rotate-refuse are different questions
about the same group; conflating them misreads both.

**And the property that makes (b) safe: rotation never MINTS.** Option (b)
widened *overwrite* authority only. Every rotation path exits early when no
secret is held — web `skip-no-secret` (`group-routing.ts:1099`), iOS
`.skipNoSecret` (`:1087-1090`), Android `SKIP_NO_SECRET` (`:953-955`) — with the
mint call strictly downstream of that return. Mint authority is decided
elsewhere and is **still owner-only**: web `ensureGroupPushRouting:738-742`, iOS
`ensureProvisioned:816`/`:823`, Android `decideEnsureAction:215`. So an admin can
replace a secret the group already has, but can never become a second minter and
so can never diverge the group's `push_gid` into two.

### Android additionally deleted a prior security hardening — deliberately

`a6ed544` **removes the strip-admins-on-unreadable-store rule** from Android's
`receiveGated`. That rule came in with the ADR-0042 **MEDIUM-1** hardening
(`21554f2`, refined `62f23ad`): when the secret-store read *threw*, it rebuilt
the authority as `OwnerLookup.Known(owner.owner)` with an **empty admin set**, so
that an unreadable store could not be mistaken for a real hole an admin was
allowed to fill.

Under (b) that premise is gone — an admin may overwrite **whatever** is stored,
provable hole or not — so retaining the rule would have refused *the very admin
rotation option (b) exists to permit*, on nothing more than a transient store
error. `receiveGated` now passes the authority through unmodified
(`GroupPushRouting.kt:499-504`, the `owner = owner` pass-through), with a
historical note at `:464-474`. **This is a
deliberate narrowing-removal of a prior security hardening and is recorded as
such.** Who may write is still decided by the role table and is still
fail-closed; an *unreadable role table* (as opposed to an unreadable secret
store) still refuses.

**The rule existed on all three clients; only its mechanism was Android-only.**
Web and iOS encoded the same premise inline in the pure classifier via a
`storedUnreadable` flag rather than by rewriting the authority. Both **kept the
parameter but demoted it**: web now uses it only to choose *which* accept to
report (`accept-admin-seed` vs `accept-admin-rotate`, `:513-517`), iOS retains it
as a diagnostic that no branch reads (`:565`). Android deleted its input
entirely. The behaviour is the same on all three; the residual asymmetry is
cosmetic.

Android also had to fix an interaction the gate change surfaced: with the admin
case now accepted, `receiveGated` began reaching `subscribeOrDefer`, whose store
read was unguarded — a throwing store escaped `receiveGated`, breaking its
never-throws contract (it runs per polled message) and skipping the teardown. The
read is now fail-soft (`:414-415`) and the apply+teardown is wrapped
(`:502-518`). iOS never had this defect.

### The trigger as built

A membership **SHRINK**, detected as a **set difference over member Ed25519
signature keys** — deliberately not counts and not leaf indices, because OpenMLS
reuses a vacated leaf for the next Add, so a remove+add in one poll batch leaves
both the count and the index set unchanged while a member really was evicted.

**Four decision outcomes** — `rotate` / `skip-no-shrink` /
`skip-unknown-membership` / `skip-not-authorized` — plus a fifth that belongs to
the IO half, `skip-no-secret`. A torn `members()` read or an empty baseline
yields `skip-unknown-membership`, **never** "everyone left"; growth yields
`skip-no-shrink`; a no-role-table group yields `skip-not-authorized`. Shrink is
tested **before** authority, so an unauthorised device on a no-shrink batch
reports `skip-no-shrink`.

Two entry points per client, both fire-and-forget: the device's **own** remove
path (web `Chat.tsx:2293-2299` after the awaited commit at `:2267-2271`; iOS
`ChatRepository.swift:917`; Android `ChatRepository.kt:842`), and **someone
else's** removal observed as a before/after diff around the poll batch (web
baseline `Chat.tsx:1516` -> diff `:1678-1687`; iOS baseline `:1697` -> diff
`:1727-1728`; Android baseline `:2057` -> diff `:2096-2102`). **No rotation on
add.**

Both load-bearing orderings hold on all three, and the *read-old-before-overwrite*
one is pinned by an explicit assertion that the teardown names the **old** id
(e.g. `apps/ios/LakkoTests/PushGroupRoutingTests.swift:1805`, "the teardown names
the OLD id"; also `:1680`).

### The receive-side teardown — what keeps rotation from being a net loss

A non-rotating member that accepts a **changed** secret retires **its own** old
gid: unsubscribe + delete the reverse-index row (web
`receivePushRoutingSecretGated:625-634` -> `retireOldPushGid:324-349`; iOS
`:468-475` -> `:365-378`; Android `:508-515` -> `:813-831`). Each client reads the
superseded secret from the snapshot it already takes **before** the persist.

Without this every member would leave one live push-gw row per rotation — the
"live cluster **plus** a frozen snapshot of every previous one" this ADR's
§ Context calls strictly worse than not rotating at all. It is required, not
hygiene. This also gives `PushClient.unsubscribeGroup` its **first real caller**
on every client (web `group-routing.ts:336`, iOS `PushGroupRouting.swift:220`,
Android `ChatRepository.kt:1670`), retiring the dead-code finding in G-36
residual 2.

A per-group in-flight rotation lock coalesces concurrent attempts (web
`group-routing.ts:1029`, iOS `:191`, Android `:895`), separate from the mint lock.

### Corrections to this ADR's own prose

Implementation contradicted the ADR in five places. Each is corrected inline
above rather than left to propagate:

1. **The durable pending-rotation record was NOT built, deliberately** — its
   justification went stale between authoring and implementation (§ Durability).
   The remaining uncovered case is narrower than the ADR's, and the record it
   needs is **lighter**: only the *old gid awaiting retirement*.
2. **Option (b) was ratified, not the recommended (a)** (§ The genuine fork).
3. **`reject-admin-overwrite` no longer exists** — it is `accept-admin-rotate`.
   Two arguments built on it lapsed: the "group would split" half of the
   never-rotate-on-add reasoning (§ Rotate on removal only) and the second ground
   in § Alternatives. Both conclusions survive on their other, stronger ground.
4. **The step 1-8 flow omitted its own first exit** — no secret held is a
   **no-op**, never a mint (§ The per-client flow).
5. **Most `apps/` anchors drifted.** Every symbol name is still right; the lines
   moved, by between 8 and 297, because the implementation grew the files it
   cites. Re-verified, authoring-time -> current:

   | Anchor as written | Current |
   |---|---|
   | `group-routing.ts:329-375` (verdict core) | `:468-519` |
   | `group-routing.ts:271-296` (verdict matrix) | table at `:444-452`, doc-comment `:437-466` |
   | `group-routing.ts:509` `ensureGroupPushRouting` | `:712` |
   | `group-routing.ts:577` `redistributeSecretToGroup` | `:856` |
   | `Chat.tsx:2191` `onRemoveMember` | `:2229` |
   | iOS `PushGroupRouting.redistribute:474` | `:668` |
   | Android `GroupPushRouting.redistribute:387` | `:684` |
   | Android `ChatRepository.removeMemberFromGroup:750` | `:758` |

   **One cited anchor did NOT drift and was correct as written:** iOS
   `ChatRepository.removeMemberFromGroup:841` is still `:841`. The `crates/`,
   `services/push-gw`, and ADR-0042 self-citations were all re-verified and are
   **accurate as written**.

Additionally, § Consequences' claim that there is "**no change to the
`lakko-security`-reviewed receive gate** and **no new authority** anywhere" was
conditioned on option (a) and **does not hold under the ratified (b)**: the gate's
admin row changed and admins gained rotate authority.

### `lakko-security` review — DONE, APPROVE-WITH-NOTES

This ADR states that option (b) "**must** go past `lakko-security` before merge",
because it re-opens the MEDIUM-1 fix's authority model and widens a documented DoS
residual. **That review was run on the landed arc and is the precondition
satisfied.** Verdict: **APPROVE-WITH-NOTES — no CRITICAL, no HIGH, and no
invariant #2 / #3 / #4 break on the live path.** One blocking item (F1).

| # | Sev | Finding | Disposition |
|---|---|---|---|
| F1 | MEDIUM | **The coalescing rotation lock folded a second, distinct removal into an in-flight rotation** on web + iOS — so a second evictee removed while a rotation was still in flight was handed the **new** secret, defeating the rotation for that member. Android was already correct: its serializing `Mutex` queues the second rotation instead of coalescing it | **FIXED** by adopting Android's serializing lock on both platforms — web in `935250a`, iOS in `a6ed544` (a per-group chain of `Task`s, the Swift counterpart of the Android `Mutex`). Both landed and verified. The blocking item |
| F2 | MEDIUM | **Concurrent rotators can strand a device on a superseded secret** — a rotator applies its own mint at *local* time rather than at its journal position, and under (b) one removal yields N+1 rotations. This is the same defect as the convergence correction in § The genuine fork | **RECORDED, not fixed** (residual 7 below, G-36 residual 7). Its **web-only stale-role sub-case IS fixed** |
| F3 | LOW | **The post-Remove-epoch ordering (C-61) has no test on any client** — structural enforcement only, contrary to this ADR's own "must be asserted in a test" | **RECORDED** (residual 6 below, G-36 residual 6) |
| F4 | LOW (web) | **The receive-side teardown could precede the new subscription**, briefly leaving the device on neither gid | **RECORDED as a follow-up** |
| F5 | LOW | **A raw NUL byte committed in `apps/web/src/push/wake-target.test.ts`** (byte 4159, from `4e1c8a0`) — git treats the file as binary, so `grep -r` silently skips it; a recurrence of the ADR-0042 **MEDIUM-3** class | **Being fixed** in the working tree |
| F6 | — | **The C-60 / G-36 doc corrections** this reconcile makes | **DONE** (this change) |

**Re-review (pre-merge), APPROVE WITH NOTES / merge-ready.** A second `lakko-security`
pass over the landed arc confirmed F1 fixed on all three and raised four further
items, none blocking:

| # | Sev | Finding | Disposition |
|---|---|---|---|
| F-A | — (confidentiality, non-blocking) | **A stale `pending` re-broadcast can silently UNDO a rotation, permanently.** No malice, no crash: a failed post-add redistribute leaves the pre-rotation secret in a `pending` row; another admin rotates to `S1`; the first device re-broadcasts the **old** secret before applying `S1`, every member accepts it and **retires `gid(S1)`**, migrating the group back onto the id the evicted member still derives. No further shrink ⇒ no further rotation. **Root cause: the receive gate has no recency or monotonicity notion at all.** Worst case is *exactly* the pre-ADR-0043 status quo, so it fails **to** the old behaviour, never below it | **RECORDED** (residual 9 below; full mechanics + fix directions in **G-36 residual 9**). Fix is a `crates/lakko-contacts` envelope change (monotone counter / minting epoch) or a narrow client-side `pending`-clear |
| F-D | LOW (availability) | **The stale-role window is narrowed, not eliminated, on all three** — the role is captured *before* the rotation queues behind a predecessor, so a demotion landing during the queue wait rotates on a stale role and the rotator strands itself | **RECORDED** (residual 7 below). Self-heals at the next authorised envelope |
| F-E | LOW | **Rotation and receive are not mutually excluded** — `receivePushRoutingSecretGated` takes no rotation lock, so a peer's envelope can interleave with a local rotation and leave an intermediate orphan gid | **RECORDED** (residual 5 below). Same class as the existing orphan residual, and never an id an evicted member knows — every candidate is post-removal |
| F-G | — | **Residual numbering diverged** between this ADR (6 = concurrent-rotators, 7 = untested-ordering) and G-36 (the reverse) | **FIXED** (this change) — this ADR now follows G-36's numbering: **6 = untested ordering, 7 = concurrent rotators** |

**Two explicit judgements the review recorded**, both of which this ADR adopts:

1. **Accepting the orphaned-old-gid residual is defensible, and the durable
   pending-rotation record should NOT be built** (§ Durability). The orphan is
   inert and what it leaks is bounded; a persist on a hot path plus a launch sweep
   is not proportionate to reclaiming it.
2. **`POST /push/group/subscribe` carries no membership check and
   `POST /push/group/unsubscribe` carries no ownership proof.** push-gw cannot
   perform either — it is opaque to group membership by design (invariant #2) — so
   possession of the secret **is** the authorisation to subscribe, and the claim
   that "no remaining member can tear down the ex-member's row" rests on **the
   device pseudonym never being published**, not on server-side authorization.
   That is a confidentiality assumption about an identifier, not an access-control
   guarantee, and it is stated here rather than left implicit.

### Residuals — as built

Numbered to match **G-36**'s residual list one-for-one (`lakko-security` **F-G** —
the two diverged at 6/7 and are now aligned), so a cross-reference from either side
lands on the same item.

1. **The co-membership cluster is still materialised** (G-36 residual 1).
   Rotation *renames* the cluster; the provider re-links across the boundary by
   token-set intersection, and the set difference even **names the evicted
   device** — arguably a signal the stable id did not hand over so legibly. Needs
   per-device-blinded routing ids.
2. **The ex-member's push-gw row is never deleted, only rendered INERT.** Nobody
   else can delete it (`group_unsubscribe` deletes only the `(device, group)` row
   named in the request, and a device knows only its own pseudonym) and no client
   has "you were removed" handling. The inertness *is* the win — nothing ever
   notifies the old gid again — but it is not cleanup.
3. **The admin-seed DoS generalises to admin-churn** (G-36 residual 3) — see that
   entry.
4. **Legacy / no-role-table groups** still get no push at all, and now also never
   rotate.
5. **The old gid can be orphaned — and not only by a crash.** A transient
   secret-store read failure on the receive path yields `stored = null`, which
   persists the new secret and silently skips the teardown; that is likelier than
   the crash window. **Accepted; the durable record is deliberately not built**
   (§ Durability) — the orphan is inert and leaks only "this device was once in
   some group".
   A **third** path reaches the same orphan (`lakko-security` **F-E**): rotation
   and receive are **not mutually excluded** (`receivePushRoutingSecretGated` takes
   no rotation lock on any client), so a peer's envelope can interleave with a local
   rotation and leave an intermediate gid subscribed with nothing pointing at it.
   Same class, and bounded the same way — every candidate id in that interleaving is
   **post-removal**, so it is never an id an evicted member can derive.
6. **The post-Remove-epoch ordering is untested** (`lakko-security` **F3**, LOW) —
   pinned as **C-61**, enforced structurally on all three clients, with **no test
   on any of them**, contrary to this ADR's own "must be asserted in a test". A
   refactor could restore the leak silently and invisibly at runtime. Recorded
   under **G-36**.
7. **Concurrent rotators can strand a device on a superseded secret**
   (`lakko-security` **F2**, MEDIUM — recorded, not fixed). Under (b) one removal
   yields **N+1 rotations** (the committer plus every other owner/admin via its
   poll diff), and a rotator applies its own mint at *local* time rather than at
   its journal position, so it can overwrite its own newer secret with an older
   one polled afterwards — and the row is then `distributed: true`, so nothing
   re-broadcasts. Self-heals only at the next rotation. Foreground polling
   delivers messages throughout: a wake-delivery defect, not message loss.
   (§ The genuine fork, correction note.) **The web-only stale-role sub-case is
   fixed** — true for the case it names. A **narrower stale-role window survives on
   all three** (`lakko-security` **F-D**): each client captures the role **before**
   the rotation queues behind a predecessor on the serializing lock (web
   `Chat.tsx:2297`, iOS `ChatRepository.swift:1590`, Android `:1960` / `:1997`), so
   a demotion landing during the queue wait rotates on a stale role, peers refuse,
   and the rotator strands **itself**. Availability-only, self-heals at the next
   authorised envelope.
8. **The receive-side teardown can precede the new subscription** on web
   (`lakko-security` **F4**, LOW), briefly leaving the device subscribed to
   neither gid. Recorded as a follow-up.
9. **A stale `pending` re-broadcast can silently UNDO a rotation, permanently**
   (`lakko-security` **F-A** — the substantive re-review finding; full mechanics in
   **G-36 residual 9**). No malice and no crash required: a device whose post-add
   redistribute broadcast **failed** holds the pre-rotation secret in a `pending`
   row; another admin rotates the group to `S1`; the first device runs
   `ensureGroupPushRouting` **before** applying the `S1` envelope, and its
   any-holder re-broadcast branch re-sends the **old** secret. Every member accepts
   it (`accept-owner` / `accept-admin-rotate`), persists it, **and retires
   `gid(S1)`** — migrating the group back onto the id the evicted member still
   derives. No further shrink means no further rotation: **permanent, not
   transient.** **Root cause: the receive gate has no recency or monotonicity
   notion at all** — `routingSecretVerdict` and its twins ask only *who sent it*,
   never *is it newer*, so a **retry** of the current secret and a **replay** of a
   superseded one are byte-indistinguishable. Web narrows the window
   (`Chat.tsx:2497` awaits `refresh()` before provisioning at `:2504`, though
   `refresh()` swallows its own errors at `:1694-1698`, so a transient poll failure
   re-opens it); iOS (`ChatRepository.swift:1471`) and Android (`:229`, `:339`,
   `:375`, `:1888`) provision on independent tasks with no such ordering, so it is
   **more** reachable there. **Not blocking** — the worst case is *exactly* the
   pre-ADR-0043 status quo (the evicted member keeps receiving wakes) plus inert
   orphan rows; it fails **TO** the old behaviour, never below it, and no member is
   downgraded to the raw `group_id`. **Fix direction (follow-up, not built):** carry
   a **monotone counter or the minting epoch in the 62-byte envelope** and refuse a
   non-increasing value — the general fix, and a **`crates/lakko-contacts` wire
   change** (version bump + fail-closed codec + the `push_routing_secret_decode`
   fuzz target + all three clients together); or, narrowly and client-side only,
   clear/skip the `pending` re-broadcast once a **different** secret has been
   accepted.
10. **A bounded one-poll-interval wake hole per rotation**, as designed.

## References

- **ADR-0042** — opaque per-group push routing id. This ADR *extends* it: the
  `push_gid` derivation, the 62-byte distribution envelope, the owner-mint rule,
  the authority-gated receive path, and the untouched push-gw all carry through.
  Revisits its ratified no-rotation policy (`:206-213`, `:130-133`, `:329-332`)
  and realises the rotation-on-removal enhancement it parked.
- **ADR-0022** — group-admin roles; the owner/admin split that creates the § fork
  (an admin may commit a removal, `crates/lakko-mls/src/group.rs:571-575`, but may
  not overwrite a stored routing secret).
- **ADR-0040** — durable pending-commit outbox; the persist-before-publish +
  ack + startup-sweep shape the pending-rotation record borrows
  (`apps/web/src/chat/commit-outbox.ts`).
- **ADR-0041** — silent-push background decrypt; the reverse-map consumer whose
  fail-closed "miss degrades to the generic banner" posture makes the route-row
  overlap a taste question rather than a security one.
- **ADR-0039** — the "extends, does not supersede" precedent this ADR's framing
  mirrors: it realises a config slot ADR-0020 explicitly parked, without
  displacing the mechanism it builds on.
- **ADR-0029** — mid-call key rotation; the ADR-0029 rekey effect
  (`apps/web/src/routes/Chat.tsx:1230-1246`) is the current consumer of the
  `membersVersion` tick this ADR clarifies is a poll signal, not a membership one.
- `services/push-gw/src/registry.rs:275-300` — the live, idempotent
  `group_unsubscribe` handler (`DELETE ... WHERE device_id = ? AND group_id = ?`,
  `:284-288`); routed at `services/push-gw/src/main.rs:250`.
- `apps/web/src/push/client.ts:204` · `apps/ios/Lakko/Push/PushClient.swift:165` ·
  `apps/android/app/src/main/kotlin/me/lakko/app/push/PushClient.kt:197` — the
  three `unsubscribeGroup` wrappers with zero call sites.
- `apps/web/src/push/group-routing.ts` — `routingSecretVerdict` (`:329`), the
  verdict matrix (`:271-296`), `ensureGroupPushRouting` (`:509`),
  `redistributeSecretToGroup` (`:577`).
- `apps/web/src/push/group-routing-store.ts:172` — `putGroupRouting`; the
  `secrets` (overwriting) vs `routes` (additive, `pushGidHex`-keyed) asymmetry.
- `crates/lakko-mls/src/group.rs:715` — `process_incoming_gated`, the removed-leaf
  surfacing precedent (`:745-759` pre-merge keys, `:761` the gate);
  `:582` `process_incoming_verifying_roles`; `:785` `export_secret`.
- **RFC 9420** §12 — MLS forward secrecy; the property that makes a post-Remove
  broadcast unreadable to the removed leaf (§ Decision, ordering (a)).
- **G-36** (`docs/security/known-gaps.md`) — residual 2 is what this ADR closes;
  residuals 1, 3, 4 and the `?wake=` loose end stay open.
