<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec — proposal-based group membership (ADR-0045 S0)

**Status:** design only. The core landed first ([ADR-0045] S1+S2, `9a60ab3`); no FFI, no client code.
**Owner surface:** cross-platform (web SolidJS / iOS SwiftUI / Android Compose).
**Pattern parent:** the existing members panel and its ADR-0027 `Join requests` section
(`apps/web/src/routes/Chat.tsx` `<section aria-label="Members">`, iOS `MembersSheet` in
`UI/Chat/ChatScreen.swift:1084`, Android `MembersSheet` in `ui/chat/ChatScreen.kt:1312`).
**Placement:** `docs/ux/` — the established home for a design-only spec that precedes
implementation (`update-available-notice.md` is the sibling and this file mirrors its
section shape). `docs/adr/` holds decisions, `docs/security/` holds invariants and gaps;
neither is edited here.

> **This spec was written after the core, not before it.** That is the debt, and §11
> records precisely what it cost: eight places where the built core contradicts or omits
> what [ADR-0045] decision 5 assumed the UI would have.

> **Revised 2026-08-19 against [ADR-0045]'s rewritten decision 2 (liveness).** The ADR now
> gives a waiting request **no deadline**: the committer duty passes to the next candidate
> when the current one is not acting, and if no candidate is running the request simply
> waits until one is. Three things this spec carried are therefore **deleted, not
> re-tuned** — the `stalled` state and its never-defined `stallThreshold`, the
> `MAX_ATTEMPTS` re-request cap, and the terminal `not applied` failure it produced — and
> §12's open list drops from five items to a shorter one, with the owner dead-end answered
> (ownership transfer is being built). Everything else stands unchanged: no optimistic
> update, no `Cancel`, no pending affordance until the quiet window elapses, and a committer
> view that is automatic and invisible.

---

## 1. User goal + entry points

Unchanged goal: an owner or admin manages who is in a group. What changes is that the
action stops being immediate. Under [ADR-0045] a member **requests** a change; exactly one
elected owner-or-admin sweeps the pending requests into a single commit. There is now a gap
between "I asked" and "it happened", and — new — any member may request to **leave**.

Entry points are all existing and unchanged:

- Members panel → `Add member`, `Remove`, `Make admin` / `Remove admin`.
- Members panel → `Approve` / `Reject` on a join request (ADR-0027).
- **New:** members panel → `Leave group`.

There is no new screen and no new navigation. Every state below renders inside the
members panel or on the control the user already touched.

### Vocabulary

User-facing copy says **request** and **change**. It never says *proposal*, *commit*,
*epoch*, *sweep* or *committer* — those are protocol words. `Request` is already the
established user word in this product (`Request to join`, `Request sent. Waiting for an
admin to approve…`).

---

## 2. The one insight that shapes everything: the common case collapses

`committer_rank` gives the **owner rank 0**, and rank 0 never waits its turn. So when the
owner is the one making the change — the overwhelmingly common case in a small group — the
owner's own device is the acting committer and sweeps its own request immediately, with no
round trip to anyone. The user experience is byte-for-byte today's: tap, done.

The pending states exist for the case where the requester is **not** the owner, or the owner
is not currently running the app. Design consequence, and it is the load-bearing one:

> **No pending affordance renders until a request has been outstanding past a short quiet
> window.** A spinner that appears and vanishes in 300 ms is worse than no spinner: it
> teaches the user that membership is slow when it is not.

The control the user touched still gives immediate feedback (§4), because the publish is a
normal network operation. What is deferred is the *persistent, group-visible* pending
state.

---

## 3. Should a membership change show a pending affordance at all?

**This is the ADR's open question. The answer is yes — deferred, in place, and never
optimistic.** Three findings drive it, in order of weight.

**(a) Optimistic removal would be a security lie.** If `Remove` drops the member from the
roster immediately, the UI asserts that they are gone while they are still cryptographically
in the group: still receiving every message, still able to decrypt, until the commit lands.
Lakko's honesty corollary to invariant #4 forbids exactly this class of claim — a UI that
says "removed" when the transport still delivers to them is the membership equivalent of a
green E2EE pill over an unencrypted call. **The member row must stay, annotated, until the
commit lands.** The same applies in reverse to `Add`: a person who has been requested is not
a member and must not appear in the members list.

**(b) Showing nothing at all is also dishonest, but only sometimes.** With the owner
online the request lands before anyone could look. With every candidate absent it sits
**indefinitely — by design, since [ADR-0045] decision 2 gives the wait no deadline** —
and a wait that is not visible is not a trade against the silent fork, it is the same
silence in a new place. "Remove Bob" appearing to work while Bob is still there next time
you open the panel is worse than any indicator.

**(c) Therefore: latent.** The state model always carries pending; the UI renders it only
past the quiet window. Below the window the user sees the control's own in-progress state
and nothing else.

**And there is no `Cancel`.** Once published, the request is in every member's proposal
store and dies only when some commit lands. The core has no withdraw and MLS offers no
mechanism for one. A `Cancel` button could not be honoured, so **it must not be built** —
a local-only cancel that leaves the request live in every peer's store would be the worst
lie in this document. The consequence is that the **confirmation step is the last exit**,
which is why §6 puts one on all three platforms rather than web only.

---

## 4. States (the one model all three clients implement)

Per-request state, keyed on a client-durable record (§9), not on the panel being open.

| State | Derived from | Renders |
|---|---|---|
| `sending` | the propose call / publish is in flight | control disabled + in-progress; nothing else |
| `pending` | published, `age < QUIET_WINDOW` | nothing beyond the control's state |
| `waiting` | published, `age >= QUIET_WINDOW` | row annotation + panel notice (§5) |
| `applied` | after a commit, the intended effect is visible in `members()` / the role table | annotation clears, roster updates. Terminal |
| `superseded` | after a commit, the effect is absent **and** the request is no longer in `pendingProposals()` | nothing — re-requested silently (§7) |
| `refused` | the propose call returned an error, on the first attempt or on an automatic re-request | inline refusal copy. Terminal (§7) |

**`waiting` has no upper bound, and that is deliberate.** An earlier revision of
[ADR-0045] split it at a `stallThreshold` into a second, escalated `stalled` state and
capped re-requests at a `MAX_ATTEMPTS` that terminated in a `not applied` failure. The
revised decision 2 deletes both, and this spec follows: **a request waits until a
candidate sweeps it, however long that takes.** The reason is that neither number was
knowable from inside a client. *The threshold elapsed* and *every admin is asleep* are the
same observation, so a `stalled` label asserted a diagnosis the client could not make; and
*the budget ran out* is likewise indistinguishable from *nobody has opened the app yet*, so
a terminal `not applied` told the user their change was unachievable when in fact it was
merely unattended. **One waiting state, no deadline, no terminal failure for want of a
committer.** The states that remain terminal — `applied` and `refused` — are the two the
client can actually observe.

`age` is measured against the **DS row timestamp**, never a local clock: the client already
receives `created_at` on its own publish response and on every fetched row
(`apps/web/src/ds/client.ts`, `DsClient.kt`, `ChatClient.swift`), and every member reads the
identical value for the identical row.

**`applied` is derived by effect, not reported.** The core hands the sweeping device a
`ProposalCommit.swept` list; every other device — including the requester — receives a
content-less `Incoming::Commit`. So after any commit the client re-reads `members()` and the
role table and asks whether its own intent is now true:

| Request | "applied" test |
|---|---|
| Remove | the target's **signature public key** is absent from `members()` |
| Add | the added **signature public key** is present in `members()` |
| Leave | our own key is absent / the group is gone locally |
| Role change | the role table equals the table we proposed |

**Compare on the signature public key, never the leaf index.** OpenMLS reuses a vacated
leaf for the next Add, which is the same reason ADR-0022 keyed the role table on the
signature key. The durable record therefore captures the target's key at request time.

---

## 5. The pending surface — placement, structure, copy

The members panel gains one notice and one section. Nothing moves; nothing is replaced.

**Panel order (all three platforms):**

1. **Pending notice** — new, top of the panel, above everything. Renders only when at
   least one request is `waiting`.
2. `Join requests ({n})` — existing ADR-0027 section, unchanged, owner/admin only.
3. **`Waiting to be added ({n})`** — new, owner/admin only, only when non-empty.
4. Add-member form + invite link — existing, owner/admin only.
5. Members list — existing, with new row annotations.
6. **`Leave group`** — new, bottom, destructive styling, hidden for the owner.

Requests-to-join stay above pending changes because they need a **decision**; pending
changes need none.

### 5.1 The pending notice (exact copy)

Two lines. Line 1 is the count, line 2 is the liveness statement. **There is one line 2,
not two** — the escalated variant went with the `stalled` state (§4).

- Count, singular: `1 membership change is waiting to apply.`
- Count, plural: `{n} membership changes are waiting to apply.`

- Line 2:
  `Membership changes are applied by the group owner or an admin. They will apply automatically when one does.`

That sentence is true at ten seconds and at ten days, which is exactly why it can be the
only one: there is no moment at which the client learns something new to say. It states
**inaction**, the only thing the client can observe — it names no person, claims no
knowledge of who is online, and blames nobody. Even under [ADR-0045] decision 2's variant 1
(a candidate heartbeat) that discipline holds: a beacon licenses "someone is active", never
"this person is online", and no copy here may imply the stronger claim. This is the same
discipline as the shipped `Request sent. Waiting for an admin to approve…`.

### 5.2 Row annotations (exact copy)

A muted secondary line under the member's existing label, in the members list:

- `Waiting to be removed.`
- `Waiting to leave.`

And in the new `Waiting to be added ({n})` section, one row per pending add, rendered with
the same identity treatment the join-request rows already use (contact display name if
known, else the short fingerprint), with the secondary line:

- `Waiting to be added.`

A pending **role change** gets no row annotation — it cannot be attributed to a member
(§11.5). It contributes to the count and shows only at panel level.

### 5.3 Controls while a request is pending

- A member with a pending removal keeps its `Remove` button **visible but disabled**.
  Hiding it would read as the permission having vanished. The annotation explains the
  disabled state.
- Re-requesting an identical change is refused **client-side, before any publish**, and
  surfaces the existing request rather than creating a second one. Two identical Remove
  proposals in one sweep is a defect, not a feature.
- The `Add member` form stays enabled — a second, different add is legitimate and composes
  into the same sweep. That composition is the entire point of [ADR-0045].

---

## 6. The proposer's view, end to end

**Tap `Remove` →** confirmation (see below) **→** control disables, in-progress **→**
one of: the roster updates within the quiet window (nothing else ever renders), or the row
annotation + panel notice appear.

**Confirmation is now required on all three platforms.** Today only web confirms
(`window.confirm("Remove this member (leaf #N)?")`); iOS and Android remove on a single
tap. Because a request cannot be withdrawn (§3), the confirm is the last exit and the
asymmetry has to go. Copy:

- Title: `Remove this member?`
- Body: `They will keep receiving messages until an owner or an admin applies the change.`
- Buttons: `Remove` (destructive) / `Cancel`

That body line is not a nicety. It is the one fact a user removing someone for a safety
reason must have, and it is the fact the old immediate-looking UI concealed.

**Leaving** (new — the core made this reachable for the first time):

- Control: `Leave group`
- Title: `Leave this group?`
- Body: `You will keep receiving messages until an owner or an admin applies your request. You will need a new invite to rejoin.`
- Buttons: `Leave` (destructive) / `Cancel`
- Pending annotation on your own row: `Waiting to leave.`
- The control is **hidden for the group owner** — the owner cannot leave, structurally and
  permanently, not transiently (§11.6). Showing a control that always refuses is worse than
  not showing it.

**Refusal copy.** The core authorises the requester locally *before* publishing, so an
unauthorised request never reaches the wire and the user gets a synchronous, specific
error. One string per core refusal, reused verbatim wherever it surfaces:

| Core refusal | Copy |
|---|---|
| add, not owner/admin | `Only the group owner or an admin can add members.` |
| remove, not owner/admin | `Only the group owner or an admin can remove members.` |
| remove targets the owner | `The group owner cannot be removed.` |
| owner self-remove / leave | `The group owner cannot leave the group.` |
| role change, not owner | `Only the group owner can change admins.` |
| unsupported / unresolvable / no role table | `This change could not be requested.` |

The last row is the fail-closed fallback. It must exist, must never leak an internal
reason, and must never be reached in normal use.

**Delayed refusal.** A request can be refused *after* the fact — the requester was demoted
while the request was in flight, so peers dropped it and it never landed. The requester
cannot observe that directly (§11.1); what happens is the automatic re-request re-runs the local
authorisation and **now** fails. The user sees the same specific string, prefixed:

> `Your earlier request was not applied. ` + the specific refusal above.

e.g. `Your earlier request was not applied. Only the group owner or an admin can remove members.`

One string table, two entry points, no second vocabulary.

---

## 7. Failure and liveness states

**Nobody acting.** The request stays `waiting`, per §5.1, for as long as it takes. No
action is offered because none exists: no cancel, no "remind an admin" (there is no channel
for it), no notification, and — since [ADR-0045] decision 2 has no deadline — no escalation
and no expiry. The request stays live and lands when any candidate next runs the app.

**Superseded.** The epoch advanced without carrying the request — it missed the sweep, or
the group moved for another reason. **Silent and automatic:** the client re-requests, and
the user is told nothing. This is right because the user's intent is unchanged and the
system is doing what they asked. There is **no attempt cap**: a request that keeps getting
superseded keeps being re-requested, and returns to plain `waiting` each time.

> **The one thing this leaves unhandled, named rather than papered over.** A cap existed
> for a second reason as well as the timing one: it bounded the [ADR-0045] residual of a
> **partially censoring committer** — one that sweeps some requests and silently drops
> others — by eventually converting the loop into a visible `This change could not be
> applied`. With the cap gone, that case now presents as an indefinite `waiting`, which is
> honest (the client genuinely cannot distinguish it from an idle group) but offers the
> user nothing at all. It also means a silent re-request loop is unbounded, and every
> re-request is a journal row held to the DS's 30-day TTL. **Mitigation, not a fix:**
> re-request only on an observed commit that superseded it (never on a timer), so the loop
> is driven by group activity rather than by the clock and cannot spin against a quiet
> group. Whether that is sufficient is §12.

**Dropped by the authorisation gate.** Reachable only as the delayed refusal in §6. A peer
that drops a request stores nothing and renders nothing — the drop is invisible on the
receiving side by design, and correctly so: it is not that peer's business.

**Sweep blocked on this device** — the one committer-specific state (§8). Copy:

- `Membership changes could not be applied. They will be retried automatically.`
- Action: `Retry now`

`Retry now` appears **only** in this failure state. It is not a manual apply button; the
normal path has none (§8).

**Group with no role table.** Every peer's receipt gate drops proposals in a group with no
role table, so the propose path would silently discard everything. The client must branch on
`groupRoles() == null` and keep the direct-commit path there, exactly as [ADR-0045] says.
No UI difference; a correctness requirement stated so nobody omits it.

**External join, after the ADR-0045 S7 convergence.** `Approve` stops meaning "added" and
starts meaning "marked for the next sweep", so an approved joiner moves from
`Join requests` into `Waiting to be added` rather than into the roster. If the approved
request is superseded, the admin cannot re-request it — only the joiner holds the material —
so it is terminal on the admin side:

- `This request expired before it could be applied. Ask them to request to join again.`

(ADR-0028's joiner already retries on a stale epoch, so in practice a new request usually
arrives on its own. The string is the fallback, not the expected path.)

---

## 8. The committer's view: automatic, and invisible

**The sweep is automatic. There is no `Apply changes` button.** Three reasons, in order:

1. A manual apply turns an availability property into a chore and makes waiting *more*
   likely, not less — [ADR-0045]'s own argument, and it is correct.
2. **It would misrepresent authority.** The committer is not approving anything. The
   authorisation already happened, at the proposal gate, against the requester's own role.
   A button labelled `Apply` reads as a decision; presenting a rubber stamp as a decision is
   the same class of dishonesty as a cosmetic security badge. (`Approve` on an external join
   *is* a real decision and keeps its button — that distinction has to stay legible, which
   is why the two sections are separate.)
3. There is nothing to decide. The sweep filter re-runs the same rule; a committer cannot
   include or exclude anything by choosing.

**What the committer sees that others do not: in the steady state, nothing.** No badge, no
"you are the committer", no count that others lack. Telling someone they hold a duty they
did not ask for, cannot decline, cannot delegate and cannot discharge by any action is
anxiety without agency.

**How do they know they are the committer? They do not, and they do not need to.**
[ADR-0045] makes the committer a **per-batch duty, not an office** — which is exactly why it
needs no handover protocol, no election event and no committer state. A UI that surfaced
the role would be inventing an office the protocol deliberately declined to create, and
would then owe the user an explanation every time it silently changed hands (owner opens the
app, owner backgrounds it, an admin's delay elapses). **The handover being invisible is the
feature.**

**The one committer-only surface: the sweep is failing on this device.** When this device
is a candidate, holds authorised pending requests past its own activation delay, and its own
sweep or publish is failing, only this device can fix it — so only this device is told, with
the §7 copy and its `Retry now`.

**Deliberate departure from decision 5.** The ADR frames `N changes waiting to apply` as "one
committer-side surface". It is not committer-side data: `pending_proposals()` is populated on
**every** member's device by the receipt gate, and `own_committer_rank` is the only
committer-specific signal the core exposes. So the count becomes the **shared** notice of
§5.1, role-gated for visibility (§10), and the committer keeps no exclusive view of it.

**No notification, ever.** A push for "changes are waiting for you" would name an undeclinable
duty, and would put per-group membership-activity timing in front of the push provider for a
non-message event. If long waits prove common, the fix is ownership transfer and more admins, not
a nag.

**A liveness fact the design must not hide from the maintainer:** the ADR-0041 background
decrypt is read-only-throwaway by construction and cannot commit, so **a backgrounded
candidate cannot sweep**. Sweeps happen only while a candidate has the app in foreground
polling, so waiting is more reachable than "their phone is on" suggests. **[ADR-0045] has
since absorbed this fact and it is now benign:** with no deadline and no attempt cap, a
longer wait is just a longer wait — it can no longer mature into a `stalled` label or into
a terminal failure the client had no basis to assert. It changes no copy; it is why the
waiting string had to be blameless in the first place, and it is why one blameless string
is now sufficient (§5.1). It also bounds decision 2's variant 1: a heartbeat can only be
emitted from the foreground, so it would correctly report *able to sweep* rather than
merely *device awake*.

---

## 9. Data the UI needs (the UX-first ask)

**Status note.** At the time of writing, S3 (FFI) is **in progress, uncommitted**, in the
working tree — `crates/lakko-ffi-uniffi/src/lib.rs` and `crates/lakko-ffi-wasm/src/mls.rs`
already carry `ProposalKind` / `ProposalRecord` / `ProposalCommit`, the three `propose_*`
methods, `pending_proposals`, `own_committer_rank`, `commit_pending_proposals`, and the
`proposal()` accessor on the processed-message projection, with wasm using the flat
`kind` string plus `add_signature_key` / `remove_target_leaf` shape this section asks for.
**So items 1–5 below are landing already and are recorded here as the requirement they
satisfy, not as new work. Item 6 and the typed refusal reason in item 1 are not in that
change and remain open.**

**FFI, both surfaces in lock-step (uniffi + wasm):**

1. `proposeAddMemberFromWire` / `proposeRemoveMember` / `proposeGroupRoles` → wire bytes to
   publish. Errors must arrive **discriminable enough to select one of the six §6 strings** —
   today `authorise` returns `Error::Group(String)`. Matching on prose across three clients
   is the drift class ADR-0044 was written about. Prefer a typed refusal reason.
2. `pendingProposals()` → `[{ senderLeaf: u32?, kind, stored }]`, where `kind` is flat and
   JSON-able for wasm (`"add" | "remove" | "leave" | "update" | "roleChange" | "unsupported"`
   plus `addSignatureKey?` / `removeTargetLeaf?`), following the SFU-frame precedent.
3. `ownCommitterRank()` → `u32?` — needed for the sweep trigger and for §8's blocked state.
4. `commitPendingProposals()` → `{ commit, welcome?, swept[] }`.
5. `Incoming::Proposal` on the processed-message projection: `{ senderLeaf, kind, stored }`,
   so the receive loop can refresh the panel and can drop a `stored == false` record without
   rendering anything.
6. **One shared definition of whatever timing the chosen liveness variant needs.**
   [ADR-0045] decision 2 leaves this open: **variant 1** (a candidate heartbeat) needs a
   beacon interval and a freshness window; **variant 2** (staggered takeover) needs one
   `step`. Either way the number belongs in **one** definition consumed over both FFI
   surfaces, never hand-copied — three clients each carrying their own copy is precisely
   the ADR-0044 drift shape. **What this item no longer asks for is a stall threshold:**
   the UI has no timing-derived label left to keep in step with the sweep trigger, because
   `waiting` is unbounded and never escalates (§4). Only the sweep *trigger* consumes the
   constant now, which makes the drift risk smaller but not zero.

**Client-durable state (per platform, not FFI).** A per-`(account, group)` pending-request
record, sibling to ADR-0040's commit outbox and subject to the same discipline: written
before the publish, surviving restart, purged on `applied` / `refused` and on `recover` /
`forgetDevice`. Fields:

`{ requestId, kind, targetSignatureKey | addSignatureKey | proposedRoleTable, dsMessageId, dsCreatedAt }`

`dsCreatedAt` is what §4's `age` is measured against, and `targetSignatureKey` is what §4's
applied-test compares on. **There is no `attempts` field** — nothing counts re-requests any
more (§7), so persisting a count would be dead state that invites someone to cap on it.

**Timing constants** (§12 — must be measured, not guessed): `QUIET_WINDOW` (UI) and the one
protocol constant decision 2's chosen variant needs. `owner_grace`, `stallThreshold` and
`MAX_ATTEMPTS` are all gone.

---

## 10. Visibility: who sees the pending set

**Rule, stated once:**

> The pending set is visible to the owner and admins. Every member additionally sees the
> status of a request they made themselves. No member is ever told about a pending change to
> another member's status unless they can act on it.

**Argued, because this is a privacy decision and not a layout one.**

The pending set is a work queue, and only owner/admins have a verb over it. A plain member
shown "Bob is waiting to be removed" learns something they cannot act on, about someone
else's standing, in a window that did not exist before [ADR-0045] created it.

**The window is the sensitive part.** Under direct commits, a removal was atomic: the target
got no warning. Under proposals there is now an interval in which the target is still fully
in the group — reading, decrypting, able to react — and knows they are on the way out. In a
group where a removal is a safety action, advertising it is a hazard. The protocol creates
the interval either way; the UI decides whether it is obvious.

Note the interval is not something the UI can conceal from a determined target: the receipt
gate stores the proposal on **every** member's device including theirs. So the choice is not
"hide it from Bob" — it is "do not build a surface that broadcasts it". Not rendering
something asserts nothing, so it breaks no honesty rule, and it matches the panel's existing
behaviour exactly: `Add member`, `Remove`, the invite block and `Join requests` are already
all role-gated today.

**The exception is non-negotiable:** a member's own request must be visible to them, or
`Leave group` becomes a button with no feedback.

**Proposals never appear in the chat timeline.** A request is not something that happened to
the group; it is an intent that may never take effect. Writing "Alice asked to remove Bob"
into a permanent, mutually-visible transcript is both the §10 leak and a durable record of an
intent that the system may discard a second later. Only outcomes belong in the timeline —
i.e. today's membership rows, unchanged. This settles the S4 note that "the `Proposal` result
stops being a content-less system row": it stops being a **timeline** row and becomes a
**panel** signal.

---

## 11. What the built core contradicts or omits in decision 5

The cost of writing this after the core rather than before it.

1. **`refused`, as described, is unreachable — and the requester could not see it anyway.**
   Decision 5 defines it as "the proposer's role changed between proposing and the sweep, so
   the gate excluded it". `group_proposals.rs`'s own module documentation refutes that
   window: OpenMLS empties the whole proposal store on every merged commit, and the role
   table can only change *by* a commit, so the table cannot move underneath a stored
   proposal. Separately, the requester never receives its own proposal back, and
   `Incoming::Commit` is a content-less unit variant — so **no remote verdict of any kind
   reaches the requester**. Refusal therefore reaches the user only as a *local, synchronous*
   error: on the first attempt, or on the automatic retry (§6, delayed refusal). The state
   survives; its delivery mechanism is completely different from what the ADR assumed.

2. **`applied` is not reported to anyone but the sweeping device.** `ProposalCommit.swept`
   is returned only to the caller of `commit_pending_proposals`. Every other device gets
   `Incoming::Commit` with no payload. `applied` must be derived by effect-diffing the roster
   on signature keys (§4). Workable, but it is client logic the ADR did not budget.

3. **The liveness half of decision 2 is not in the core.** There is no activation delay, no
   DS-row-age input and no stand-down re-read. `commit_pending_proposals` commits whenever
   an owner/admin asks, so the whole election — whichever variant is chosen — is client
   orchestration that does not exist yet. **This finding was raised against the earlier
   revision of decision 2 and the ADR has since acted on it.** The sharpest form of it was
   that `stalled` was *unimplementable*, because `stallThreshold` had no definition
   anywhere; the revised decision 2 **deleted the state rather than defining the
   threshold**, which resolves that half outright (§4). What survives is the weaker,
   permanent form: `waiting` still needs a sweep trigger to exist for it to ever end, and
   the trigger still needs the one constant decision 2's chosen variant carries (§9.6).
   That constant has not been measured either — but nothing the **UI** says now depends on
   it, which is the difference.

4. **Decision 5 omits leaving entirely, and the core just built it.** `ProposalKind::Leave`
   plus self-targeted `Remove` are authorised for any member, closing ADR-0022's deliberately
   unreachable self-remove branch. That is a **new user-facing action on every group for
   every member** with no design in the ADR. §6 designs it.

5. **A pending role change cannot be attributed to a member.** `ProposalKind::RoleChange`
   carries no target — `classify` discards the `GroupContextExtensions` payload — so a
   *received* role-change request is only describable at group level. In practice this is
   nearly invisible (role changes are owner-only and the owner is rank 0, so they sweep
   immediately), but a row-level annotation for it cannot be built without an FFI addition.

6. **The owner cannot leave — and the maintainer has chosen to fix it rather than ship
   it.** The core refuses an owner `Leave`/self-`Remove` fail-closed, with a sound
   justification (an ownerless group can never appoint another admin, because
   `set_group_roles` is owner-only). Combined with ADR-0022's deferred ownership transfer
   that made **an owner permanently bound to every group they create.** [ADR-0045] listed
   ownership transfer as "load-bearing"; **§12's escalation has been answered — ownership
   transfer is being built, and a core sprint for it is running now.** This spec does not
   design its UI: the core API does not exist yet, and designing a transfer flow against an
   unknown surface would be the exact mistake §11 exists to record. **What follows for this
   spec:** the `Leave group` control stays hidden for the owner (§6) until that lands, and
   a follow-up UX pass owns the transfer flow, the ownerless-group question and whatever
   `Leave group` becomes for an owner afterwards.

7. **`N changes waiting to apply` is not committer-side data** (§8, deliberate departure).

8. **Nothing in the ADR says there is no cancel.** The core has no withdraw and MLS offers
   no mechanism for one, so a `Cancel` affordance cannot be honoured. Recorded here
   explicitly so that nobody builds one in S4–S6 (§3).

---

## 12. Open — needs the maintainer

1. **The timing constants.** `QUIET_WINDOW` (UI) plus the one protocol constant
   [ADR-0045] decision 2's chosen variant needs. The ADR says pick them from a measurement,
   and it is right. **Nothing in this spec is blocked on either now** — the state that was
   blocked (`stalled`) is gone, and `QUIET_WINDOW` only decides *when* an annotation
   appears, not whether the spec is implementable. Down from four numbers to two.
2. **Decision 2's liveness variant** — a candidate heartbeat, or a staggered takeover?
   Mostly invisible in the UI, and this spec is written to hold either way, with one
   exception worth flagging from the UX side: **a heartbeat publishes each candidate's
   activity schedule to the whole group, continuously.** §10 already argues that the
   pending set is a privacy surface; a beacon is a second, larger one, and it is *not*
   role-gateable — every member would see it. It belongs on the maintainer's ledger for
   that reason as much as for the journal-litter one [ADR-0045] names.
3. **~~The owner dead end (§11.6).~~ Answered — ownership transfer is being built**, a core
   sprint is running, and the flow is a follow-up UX pass once the API exists.
4. **Visibility (§10).** I recommend owner/admin only, plus your own request. The
   counter-argument — that a member deserves to know a change is coming — is a values
   position, and it is the maintainer's.
5. **The unbounded re-request loop (§7).** With `MAX_ATTEMPTS` gone, a request that keeps
   being superseded is re-requested indefinitely and a partially-censoring committer now
   presents as a permanent `waiting` with no user-side signal at all. Recommended
   mitigation is in §7 (re-request only on an observed superseding commit, never on a
   timer) — enough to bound journal litter and battery, **not** enough to make censorship
   visible. Accept, or ask for a different treatment.
6. **Typed refusal reasons on the FFI (§9.1).** Prose-matching `Error::Group` strings across
   three clients would work and would be wrong. Worth the small core change.

---

## 13. Accessibility

- The pending notice is announced **politely**, using each platform's non-interrupting
  live-region mechanism. It must never interrupt — nothing about it is urgent, and it can
  change while the user is reading the roster.
- Row annotations are part of the row's accessible label, not a separate focus stop:
  `"{member label}, waiting to be removed"`.
- A disabled `Remove` on a pending row keeps its label and gains the disabled trait, so a
  screen-reader user hears *why* from the row annotation rather than losing the control.
- The `Retry now` action in §7 is a real focusable button.
- **State is never colour-only.** With `stalled` gone there is no escalation left to
  encode, so the point narrows: `waiting` is carried by the annotation's *text*, never by
  hue and never by a lone icon. A colour-blind or greyscale user loses nothing.
- Destructive confirms follow each platform's native dialog so the destructive role is
  announced.

---

## 14. Per-platform notes (only where an idiom forces a difference)

The states, transitions and copy above are identical everywhere. These are the only
divergences, and each is forced.

- **Form.** Web = inline panel section; iOS = `MembersSheet` in a `NavigationStack`;
  Android = full-width `Dialog`. Section order and copy are identical inside all three.
- **Busy convention.** Web puts `-ing…` in the button (`Adding…`, `Refreshing…`); iOS and
  Android use a spinner plus `disabled`. Keep each — it is already the established
  convention in this panel.
- **Confirmations.** Web's `window.confirm` is replaced by an in-page dialog so the body
  line in §6 reads as part of the app rather than as a browser alert, and so the
  destructive action can carry destructive styling. iOS and Android use their native
  destructive confirmation dialogs.
- **A bug this spec must fix on iOS.** `membershipError` renders inside the
  `canManageMembers`-gated `Add member` section (`ChatScreen.swift`), so a **plain member's**
  membership error is invisible. That is tolerable today because a plain member has no
  membership action; `Leave group` makes it a real defect. **The membership error slot must
  move out of the gated section on iOS.** Android already places it ungated; web renders it
  page-level below the panel and should host the new notice at panel top instead.
- **A refresh gap this spec depends on.** iOS re-reads `pendingJoins` on every poll while
  the panel is open (`sync(state:)`); Android refreshes only on panel-open and after an
  action. The pending set must refresh on **every** poll on all three — not to escalate a
  state (there is nothing to escalate to any more) but because `waiting` **clears** on the
  sweeping commit, and a panel that refreshes only on open would go on showing a request
  that has already applied. That is §3's honesty rule in reverse, and it is the worse
  direction to get wrong.

---

## 15. UX risks (flagged, not solved)

- **The wait has no user-side remedy, and now no end state either.** The notice tells the
  truth and offers nothing, indefinitely. That is honest and it is also unsatisfying; the
  real remedies are ownership transfer (now being built) and appointing more admins, both
  outside this spec. Removing `stalled` made this *more* true, not less — the earlier design
  at least changed its sentence eventually, which felt like progress while conveying nothing
  the client actually knew.
- **Partial censorship is now indistinguishable from an idle group.** A committer that drops
  one requester's changes used to surface, eventually, as `This change could not be applied`
  for that person. With the attempt cap gone it surfaces as a permanent `waiting` and
  nothing else. [ADR-0045] names this residual as bounded by nothing in the design; the UI
  inherits it and has lost the one weak signal it had. Flagged in §12.5.
- **The removal warning window (§10) is new and permanent.** No UI choice removes it.
- **Two devices, one account** (G-38 residual 2) derive the same rank and cannot be
  separated by this design. Out of scope here, unchanged by anything above.

---

[ADR-0045]: ../adr/0045-remote-commit-epoch-fork.md
