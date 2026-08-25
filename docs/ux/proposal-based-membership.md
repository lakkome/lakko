<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# UX spec — proposal-based group membership (ADR-0045 S0)

**Status:** **implemented, with named exceptions.** The core landed first ([ADR-0045]
S1+S2, `9a60ab3`), then the FFI (`c5c0788`), **ownership transfer** (`e780952`), the
**staged-sweep rollback plus the mute fix** (`fe3de6c`), the **committer beacon**
(`94825ec` / `94da492`) — and then the clients: `1de5fe7` (web), `02f0037` (the
beacon), `000d9f8` (the send lock), `1f230ab` (iOS + Android). **What this spec
designed and the clients did NOT build** is listed once, here, so a reader does not
have to reconstruct it from §11: the **ownership-transfer flow** (§11.6 — enumerated
and deliberately not designed, so this is a UX pass that has not happened rather than
an implementation that skipped it), the **typed refusal-reason table** (§9.1 / §12.6 —
the core still returns prose `Error::Group` strings, and mapping prose to fixed copy
is the drift class this spec itself flags), the **committer-side failure surface** with
its `Retry now` (§7 / §8 — as written it is gated on an "activation delay" belonging to
the *staggered* variant the maintainer did not choose, so it is unimplementable as
specified), and the **automatic re-request on supersession** (§7 — no client has the
code; a superseded request is dropped and the user must ask again). One more, which is
a per-platform gap rather than a missing design: **`Leave group` shipped on web only**
(§5, §12.3), so the member-facing half of what [ADR-0045] unlocked is reachable on one
client of three.
**Owner surface:** cross-platform (web SolidJS / iOS SwiftUI / Android Compose).
**Pattern parent:** the existing members panel and its ADR-0027 `Join requests` section
(`apps/web/src/routes/Chat.tsx` `<section aria-label="Members">`, iOS `MembersSheet` in
`UI/Chat/ChatScreen.swift:1084`, Android `MembersSheet` in `ui/chat/ChatScreen.kt:1312`).
**Placement:** `docs/ux/` — the established home for a design-only spec that precedes
implementation (`update-available-notice.md` is the sibling and this file mirrors its
section shape). `docs/adr/` holds decisions, `docs/security/` holds invariants and gaps;
neither is edited here.

> **This spec was written after the core, not before it.** That is the debt, and §11
> records precisely what it cost: **twelve** places where the built core contradicts or
> omits what [ADR-0045] decision 5 assumed the UI would have. Items 9 and 10 were added on
> 2026-08-20 and items 11 and 12 on 2026-08-24; none was caught by the spec's own writing —
> they came out of later core sprints, which is the debt compounding rather than being paid
> down.

> **Revised 2026-08-25 against the three client sprints.** The spec is now measurable
> against code rather than against intent, and it comes out well: §4's state table,
> §5's copy, §6's flow, §8's automatic-and-invisible committer and §10's visibility
> rule were all built as written, and the `QUIET_WINDOW` §12.1 left open is picked
> (3 s, and each client says in its own source that it is **chosen, not measured** —
> which is the honest form). **Three corrections it earned, and one thing it got
> wrong.** The wrong one is §5.3: *"a second, different add composes into the same
> sweep"* is true of adds and **false if generalised to role changes** — RFC 9420
> permits one `GroupContextExtensions` proposal per commit, so a sweep silently
> discards the second and `ProposalKind::RoleChange` carries no payload for a client to
> repair it from. §5.3 is amended below. The corrections: §11.11's *the beacon has no
> emitter* **closes** (all three clients emit, store and fold); §11.9's staged-sweep
> obligation **is discharged** on all three, and how is worth recording because it is
> not obvious; and §11.3's permanent form finally closes too, since `waiting` now ends
> without anyone being asked. What did **not** get built is listed in the status line
> above rather than buried here.

> **Revised 2026-08-24 against two further core sprints.** The FFI back-fill landed
> (`6c77cd6`), so §11.10 **closes** and §11.6's transfer flow is now buildable. Decision
> 2's liveness variant is **answered** — the maintainer chose the candidate heartbeat and
> it is built in `lakko-contacts` and on both FFI surfaces (`94825ec` / `94da492`), so
> §9.7's shared-timing ask is satisfied and §12.2 closes; both are annotated with the
> honest qualification that the two constants are **chosen, not measured**. Two new §11
> items: **11** the beacon has no emitter and no fold caller, so the mechanism is inert and
> the whole election is still unbuilt client work; **12** a beacon otherwise renders as a
> chat bubble through `parse_body`'s lossy-UTF-8 fallback, which all three clients now
> filter on both the timeline and the notification path. Nothing in §4's state table, §5's
> copy or §10's visibility rule moves — and §10's discipline now governs the beacon too:
> it licenses "a candidate is active", never "this person is online".

> **Revised 2026-08-20 against the built core.** Three things settle: the **owner dead end
> is closed in the core** (ownership transfer landed, `e780952`) so §11.6 and §12.3 change
> from open questions into a design task with a known API — affordance, member picker, the
> `OutgoingOwner` choice, and an irreversibility confirmation, enumerated in §11.6 and
> deliberately not designed here; a **staged sweep freezes a group's own membership
> operations durably**, which is a hard new implementation obligation for S4-S6 (§11.9);
> and the ~~**FFI reaches neither of those two core surfaces**, which makes an FFI back-fill
> a prerequisite for S4~~ — **the back-fill has since landed (`6c77cd6`); see the 2026-08-24
> banner above and §11.10.** Nothing in §4's state table, §5's copy or §10's visibility rule
> moves.

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
   **Built on web only** (2026-08-25); iOS and Android shipped items 1-5 and not this one.

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

  > **Amended 2026-08-25: true for adds, false for role changes, and the difference is
  > not a UI nicety.** A role change is a read-modify-write over the role table. On the
  > old direct path the per-group lock sufficed, because the first toggle's *commit*
  > updated the table and the second read it. A **proposal** changes nothing until it is
  > swept, so two toggles a moment apart both read the same committed table and each
  > proposes a table carrying only its own change — and RFC 9420 permits at most one
  > `GroupContextExtensions` proposal per commit, so the sweep keeps one and **silently
  > discards the other**. Serializing cannot fix it (there is nothing for the second read
  > to observe) and the client cannot repair it (`ProposalKind::RoleChange` carries no
  > payload, so it cannot read back what it proposed). The honest answer is to **refuse
  > the second and say so**, which web and iOS do:
  > `A role change is already waiting to apply. Wait for it to land, then try again.`
  > The window is one sweep — the owner is rank 0 and sweeps immediately — so this is
  > momentary in the ordinary case and a real wait only when the committer is absent,
  > which is precisely when a silent loss would be worst. **Android does not carry the
  > refusal**, so a second concurrent toggle there still loses one silently.

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
- The control is **hidden for the group owner** — the owner cannot leave *while owner*
  (§11.6). ~~structurally and permanently, not transiently~~ — **that is no longer the
  right word.** Ownership transfer is built, so the owner's inability to leave is a
  **precondition, not a dead end**: transfer first, then leave as an ordinary member.
  Hiding the control is still right for an owner who has not transferred (a control that
  always refuses is worse than no control), but the hidden state now needs a way out —
  which is the transfer flow this spec does not yet design (§11.6).

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

> **NOT BUILT (2026-08-25).** No client re-requests. All three prune the observation of a
> request that has left the core's pending set — swept, superseded, or the group moved —
> and stop there, so a superseded request is simply **dropped** and the user must ask
> again. The design above is still the right one and nothing built contradicts it; it
> has no implementation. The effect is a functional gap rather than a safety one: the
> user sees a request that quietly did not happen, which is the same thing they see when
> nobody sweeps. It is also why the re-request-only-on-an-observed-superseding-commit
> mitigation below has never been exercised — there is no loop yet to bound.

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

> **NOT BUILT (2026-08-25), and as written it is unimplementable.** §8 gates this surface
> on the device "holding authorised pending requests past its own **activation delay**",
> and an activation delay belongs to decision 2's **staggered** variant — which the
> maintainer did not choose. Under the heartbeat there is no delay to be past. What the
> clients do instead is retry silently: a sweep that cannot complete leaves itself staged
> and is re-attempted at the tail of the next poll, and an unreadable verdict is logged
> rather than surfaced, on the ground that nothing the user did failed and the request is
> still live. That is a defensible reading of the same intent, but it is **not** what this
> section specifies, and the specification needs re-deriving against the chosen variant
> before anyone builds to it.

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

> **Unimplementable as written, and unbuilt — see the §7 note.** The "activation delay"
> is the staggered variant's constant and the maintainer chose the heartbeat, so the
> trigger this paragraph names does not exist. The rest of §8 was built exactly as
> specified: there is no `Apply changes` button anywhere, no committer badge, no count
> that others lack, and no notification.

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

**Status note (updated 2026-08-20).** S3 (FFI) **landed** as `c5c0788` —
`crates/lakko-ffi-uniffi/src/lib.rs` and `crates/lakko-ffi-wasm/src/mls.rs` carry
`ProposalKind` / `ProposalRecord` / `ProposalCommit`, the three `propose_*` methods,
`pending_proposals`, `own_committer_rank`, `commit_pending_proposals`, and the
`proposal()` accessor on the processed-message projection, with wasm using the flat
`kind` string plus `add_signature_key` / `remove_target_leaf` shape this section asks for.
**So items 1–5 below are satisfied and are recorded as the requirement they meet, not as
new work.** ~~Still open: **item 6** … **item 7** …~~ **Updated 2026-08-24: items 6 and 7
are satisfied too.** Item 6's staged-sweep and ownership-transfer surfaces landed in the
back-fill (`6c77cd6`); item 7's shared liveness timing landed with the committer beacon
(`94825ec` / `94da492`) as `committerBeaconIntervalMs` / `committerBeaconFreshnessWindowMs`
/ `committerBeaconWorstCasePollMs` on both surfaces, alongside the fold, the throttle and
`Group.committerRank` — **exposed but not yet called by any client** (§11.11). **Still
open:** the typed refusal reason in item 1, and `QUIET_WINDOW` (§12.1).

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
6. **The two surfaces S3 did not carry** (added 2026-08-20; see §11.10):
   `stagePendingProposals()` -> the same `{ commit, welcome?, swept[] }` as item 4 but
   **without merging**, plus `hasStagedSweep()`, `mergeStagedSweep()` and
   `abandonStagedSweep()`; and `transferOwnership(newOwnerSignatureKey, outgoing)` /
   `proposeOwnershipTransfer(...)` with an `OutgoingOwner` enum. Both in lock-step on
   uniffi and wasm. Without the first, §8's committer view can only be built on the
   eager-merge path; without the second, §11.6's transfer flow cannot be built at all.

7. ~~**One shared definition of whatever timing the chosen liveness variant needs.**~~
   **SATISFIED — the maintainer chose variant 1 (the candidate heartbeat) and it is
   built.** [ADR-0045] decision 2 no longer leaves this open. The two numbers variant 1
   needs live in **one** definition and are consumed over **both** FFI surfaces exactly as
   this item asked: `committerBeaconIntervalMs()` (60 s) and
   `committerBeaconFreshnessWindowMs()` (150 s), plus
   `committerBeaconWorstCasePollMs()` (30 s), which is a term of the window's derivation
   and also a **constraint on the client** — a group poll slower than it invalidates the
   window. Nothing is hand-copied. **Honest qualification:** the numbers are **chosen, not
   measured**, contrary to §12's ask, and the core says so in those words; what stands in
   for a measurement is a budget argument (the beacon's latency is not interactive, so the
   interval is sized for journal litter and for how finely a candidate's activity schedule
   is exposed). A later measurement can overturn either. **Variant 2 remains the
   documented fallback** and would still need its own `step`. **What this item no longer
   asks for is a stall threshold:** the UI has no timing-derived label left to keep in step
   with the sweep trigger, because `waiting` is unbounded and never escalates (§4). Only
   the sweep *trigger* consumes these constants — and **that trigger is not built**: no
   client emits a beacon or folds a verdict yet, so the constants are exposed and unused.

**Client-durable state (per platform, not FFI).** A per-`(account, group)` pending-request
record, sibling to ADR-0040's commit outbox and subject to the same discipline: written
before the publish, surviving restart, purged on `applied` / `refused` and on `recover` /
`forgetDevice`. Fields:

`{ requestId, kind, targetSignatureKey | addSignatureKey | proposedRoleTable, dsMessageId, dsCreatedAt }`

`dsCreatedAt` is what §4's `age` is measured against, and `targetSignatureKey` is what §4's
applied-test compares on. **There is no `attempts` field** — nothing counts re-requests any
more (§7), so persisting a count would be dead state that invites someone to cap on it.

**Timing constants** (§12 — must be measured, not guessed): `QUIET_WINDOW` (UI) and the
protocol constants decision 2's chosen variant needs — **now settled as variant 1's beacon
interval and freshness window, exposed on both FFI surfaces and CHOSEN rather than
measured** (see item 7). `owner_grace`, `stallThreshold` and `MAX_ATTEMPTS` are all gone.

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

The cost of writing this after the core rather than before it. Items 1-8 are the
original tally against [ADR-0045] decision 5. **Items 9 and 10 came out of two core
sprints that landed after this spec was written** — the staged-sweep rollback and
ownership transfer — and are not decision-5 contradictions at all: they are
obligations and gaps the client work inherits. They are kept in the same list because
the list's real subject is *what S4-S6 must know that the ADR did not say*.

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
   the trigger still needs the constants decision 2's chosen variant carries (§9.7).
   Those constants have not been measured either — but nothing the **UI** says now depends
   on them, which is the difference. **Update (2026-08-24): the variant is chosen (1, the
   heartbeat) and its core half is built** — the beacon envelope, the acting-committer fold
   and the emit throttle are in `lakko-contacts` and on both FFI surfaces, and the fold
   does take the DS-row age as its freshness input, which this finding said was absent.
   ~~**The finding still stands in its permanent form**, for a narrower reason than it gave:
   not that the core lacks the decision, but that **nothing calls it**.~~ **CLOSED
   (2026-08-25).** All three clients emit a beacon, store observations and evaluate the
   fold at verdict time, and the stand-down re-read is written on all three — so `waiting`
   now ends without anybody being asked to do anything, which is the whole of what this
   finding was about. Two notes for a reader of the built code rather than the design: the
   clients do **not** call `commit_pending_proposals` (it eager-merges, which a committer
   that can lose a race must not do) but the staged pair; and the stand-down was built one
   notch more conservative than the ADR specified, standing down on **any** unapplied
   journal row rather than only a commit, because standing down costs one retry while
   staging on an unapplied commit costs a commit every peer drops.

4. **Decision 5 omits leaving entirely, and the core just built it.** `ProposalKind::Leave`
   plus self-targeted `Remove` are authorised for any member, closing ADR-0022's deliberately
   unreachable self-remove branch. That is a **new user-facing action on every group for
   every member** with no design in the ADR. §6 designs it. **Built on web only
   (2026-08-25)** — the natives mirrored the rest of the membership surface and left this
   out, so the "every group for every member" claim is currently true of one client.

5. **A pending role change cannot be attributed to a member.** `ProposalKind::RoleChange`
   carries no target — `classify` discards the `GroupContextExtensions` payload — so a
   *received* role-change request is only describable at group level. In practice this is
   nearly invisible (role changes are owner-only and the owner is rank 0, so they sweep
   immediately), but a row-level annotation for it cannot be built without an FFI addition.

6. **~~The owner cannot leave~~ — RESOLVED IN THE CORE; the UI is owed.** The core still
   refuses an owner `Leave`/self-`Remove` fail-closed, with the same sound justification
   (an ownerless group can never appoint another admin, because `set_group_roles` is
   owner-only). What has changed is that this is no longer a **dead end**: ownership
   transfer landed (`e780952`), and the refusal is now a **precondition** — transfer
   first, then leave as an ordinary member. The core deliberately keeps the two as
   separate commits and cannot fuse them, because both proposals are judged against the
   pre-merge table in which the transferor is still the owner, so a fused `Leave` half is
   refused.

   **The API this spec now has to design against:**

   - `transferOwnership(newOwnerSignatureKey, outgoing)` and its proposal twin
     `proposeOwnershipTransfer(...)` — owner-only, and refused locally with a clear
     error when the named key is not a member, is already the owner, or is not 32 bytes.
   - `OutgoingOwner` — `Admin` (the default, and the one a client should pre-select) or
     `Member`. The core states this rather than defaulting silently *because either
     silent default is wrong in one direction*: keeping admin leaves an unwanted
     administrator in a group the outgoing owner meant to walk away from; dropping to
     plain member strips them, in one irreversible commit, of every power they had in a
     group they created.

   **What is owed, and deliberately not designed here** (a follow-up UX pass owns it):

   - the **affordance** — where transfer lives in the members panel, and how it relates
     to the `Leave group` control it unblocks;
   - the **member picker** — choosing the new owner, on the same identity treatment the
     roster already uses (contact display name if known, else the short fingerprint),
     keyed on the **signature public key** and never the leaf index;
   - the **outgoing-owner choice** — `Admin` vs `Member`, presented as a real decision
     with its consequence stated, not as a checkbox;
   - an **irreversibility confirmation**, which this one genuinely needs: **nothing can
     undo a transfer.** A role change is owner-only, so the moment the commit lands the
     transferor cannot take the group back, cannot restore their own owner slot, and
     cannot even demote themselves back without the new owner's cooperation. That is a
     stronger claim than any other confirm in this spec — `Remove` and `Leave` are both
     redoable — and the copy must say so plainly rather than borrowing destructive
     styling and hoping it reads;
   - and what `Leave group` becomes for a former owner afterwards (it becomes ordinary,
     which is the point, but the transition needs to be legible).

   The `Leave group` control therefore stays hidden for the owner (§6) until the transfer
   flow ships — otherwise the owner is offered a way out with no way to reach it.

7. **`N changes waiting to apply` is not committer-side data** (§8, deliberate departure).

8. **Nothing in the ADR says there is no cancel.** The core has no withdraw, so a `Cancel`
   affordance cannot be honoured. Recorded here explicitly so that nobody builds one in
   S4–S6 (§3). *Precision, since [ADR-0045] originally stated this too strongly:* OpenMLS
   **does** expose a local single-proposal eviction (`MlsGroup::remove_pending_proposal`).
   What does not exist is a **wire** withdraw — evicting locally would leave the request
   live in every peer's store, which is precisely the lie §3 refuses to build. The
   conclusion is unchanged; only the supporting fact needed correcting.

9. **A staged sweep freezes that group's own membership operations, and the freeze is
   durable — a new, hard client obligation.** `stagePendingProposals` leaves the group at
   epoch N holding an un-merged commit, and OpenMLS refuses every further `propose_*`,
   add, remove and `self_update` on that group until it is adjudicated. The staged commit
   is **persisted**, so a client that stages and never reaches a verdict leaves membership
   frozen **across restarts**, not merely until the process dies. **The sequence S4-S6 must
   implement is: durably record -> publish -> read the journal verdict -> `mergeStagedSweep`
   or `abandonStagedSweep`.** Incoming messages are unaffected throughout, so the chat keeps
   working and the freeze is invisible to the user — which is exactly why it must be got
   right in code rather than caught in QA. Nothing in this spec's §4 state table changes;
   this is an implementation obligation, not a state.

   **DISCHARGED (2026-08-25) on all three, and how is worth recording.** Every sweep
   adjudicates what it staged; an owed verdict is resolved at restart through ADR-0040's
   existing `sweepPendingCommits` seam rather than a second restart path; and the
   owed-verdict check runs **ahead of** the acting-committer rank check — because an admin
   can be **demoted while holding a durable staged sweep**, and gating the resolution
   behind the rank check would freeze that device permanently. Re-publishing is safe:
   peers refuse a demoted author's commit, so it loses and is abandoned, which is the
   unwedge. One further rule the obligation implies and this item did not spell out: an
   **unreadable** verdict leaves the sweep staged rather than resolving it either way —
   merging on an unread verdict is the eager shape the arc removes, and abandoning would
   discard a commit already on the wire.

10. ~~**The FFI does not yet reach either of the two newest core surfaces.**~~ **CLOSED —
    the back-fill landed (`6c77cd6`).** The finding read: *"neither FFI surface exposes
    `stagePendingProposals` / `hasStagedSweep` / `mergeStagedSweep` / `abandonStagedSweep`,
    nor `transferOwnership` / `proposeOwnershipTransfer` / `OutgoingOwner`"* — true when
    written, and all seven are now on both surfaces in lock-step. Its two consequences for
    this spec are lifted: §8's committer view can be built on the staged path rather than
    the eager-merge shape a non-owner committer must not use, and **§11.6's transfer flow
    is now buildable**. Kept rather than deleted because the prerequisite ordering it
    states is still how S4 depends on the FFI.

11. ~~**The committer beacon is on the FFI, and nothing calls it.**~~ **CLOSED
    (2026-08-25) — everything this item said S4-S6 must inherit was built, in the shape it
    named.** Each client emits a throttled beacon gated on `shouldEmitBeacon` **and** on
    being foregrounded, keeps a per-`(group, signature key)` observation store fed by the
    `committerBeacon` receive event's sender leaf and DS `createdAt`, and evaluates the
    fold at verdict time with ranks **re-derived** through `Group.committerRank` — never
    cached at receipt. Two details the item did not anticipate: emission is driven by a
    **dedicated ticker rather than the poll**, because a beacon's meaning is a cadence and
    web's poll is event-driven; and an **unparseable row timestamp is dropped rather than
    stored as fresh**, since a beacon stored fresh could never age out and the observer
    would defer to it forever. The one thing that did **not** come out as designed is a
    deployment property rather than a UI one: the freshness window is
    `2 x interval + worst-case poll` and the web client has **no periodic poll**, so a web
    takeover can lag past the window; both natives long-poll and meet it. For this spec
    that changes nothing — the beacon still has, and must keep, no UI surface (item 12). The
    original text follows. Decision 2's variant 1
    landed in the core and on both surfaces (`94825ec` / `94da492`): `composeCommitterBeacon`,
    `decodeCommitterBeacon`, `actingCommitterVerdict`, `shouldEmitBeacon`, the four timing
    constants, and `Group.committerRank(signatureKey)` for ranking a beacon's
    MLS-authenticated sender. **The emitter and the observation store are not built** —
    `composeCommitterBeacon` has zero production call sites, so the mechanism is inert.
    What S4-S6 inherit: a throttled emit gated on `shouldEmitBeacon` and on the client
    being **foregrounded** (a beacon must mean *able to sweep*, and the ADR-0041 background
    decrypt structurally cannot commit — C-59); a per-`(group, signature key)` observation
    store fed by the `committerBeacon` receive event each client already surfaces with its
    sender leaf and DS `createdAt`; and the fold evaluated at verdict time with ranks
    **re-derived** through `Group.committerRank` against the current role table — never a
    rank cached at receipt, because a rank is a *position* in the table's order and any
    role change renumbers it.

12. **The beacon collides with the chat parser, and all three clients now filter it.**
    Not a UX decision so much as a UX hazard that was closed before it shipped: a beacon is
    an ordinary MLS application message, and `parse_body`'s deliberate lossy-UTF-8 legacy
    fallback degrades it to a **chat bubble reading `lakko/committer-beacon/v1`** plus two
    control bytes — on the timeline and in a push notification. Filter arms landed on six
    receive paths (foreground and background, three platforms) in `5d6af94` / `f53d11a`. A
    recognised beacon surfaces as its **own event** rather than being dropped, because the
    sender leaf and the row age the fold needs exist only at that point in the chain. **For
    this spec that means the beacon has, and must keep, no UI surface at all** — §10's
    discipline applies to it verbatim: it licenses "a candidate is active", never "this
    person is online", and no copy may imply the stronger claim.

---

## 12. Open — needs the maintainer

1. **The timing constants.** `QUIET_WINDOW` (UI) plus the protocol constants
   [ADR-0045] decision 2's chosen variant needs. The ADR says pick them from a measurement,
   and it is right. **The protocol half is now settled but NOT measured:** variant 1's
   `committerBeaconIntervalMs` (60 s) and `committerBeaconFreshnessWindowMs` (150 s) are
   defined once and exposed on both FFI surfaces, and the core states plainly that they are
   **chosen, not measured** — backed by a budget argument (journal litter and how finely a
   candidate's activity schedule is exposed) rather than by data, and overturnable by a
   later measurement. ~~`QUIET_WINDOW` is still unpicked.~~ **Picked: 3 seconds**, on all
   three clients, and each says in its own source that it is **chosen, not measured** —
   the same honest form the protocol constants use. **Nothing in this spec was blocked on
   either** — the state that was blocked (`stalled`) is gone, and `QUIET_WINDOW` only
   decides *when* an annotation appears, not whether the spec is implementable.
2. **~~Decision 2's liveness variant~~ — ANSWERED: the candidate heartbeat (variant 1),**
   chosen by the maintainer and built in the core and on both FFI surfaces; variant 2 is
   kept as the documented fallback. It is invisible in the UI, and this spec holds unchanged
   — but **the exception this item flagged was accepted, not resolved**, and it should stay
   on the ledger in that form: **a heartbeat publishes each candidate's activity schedule to
   the whole group, continuously**, and it is *not* role-gateable — every member sees it.
   §10 argues the pending set is a privacy surface; the beacon is a second, larger one. The
   maintainer's stated ground for accepting it is that online/offline visibility is ordinary
   messenger functionality. The 60 s interval is where that cost is actually priced: a
   shorter one buys a finer-grained schedule for no benefit anyone is waiting on, since
   nothing interactive waits on a beacon.
3. **~~The owner dead end (§11.6).~~ Closed in the core — the design work is now
   schedulable, not blocked.** Ownership transfer landed (`e780952`) and its API is known
   (§11.6). Nothing here waits on the maintainer any more; what it waits on is a UX pass
   for the transfer flow — affordance, member picker, the `OutgoingOwner` choice, and an
   irreversibility confirmation — which §11.6 enumerates and deliberately does not design.
   ~~**Sequencing question that is the maintainer's:** whether that pass runs before S4-S6
   or alongside them.~~ **Answered by events: neither.** The client sprints did not build
   the transfer, so the owner dead-end is closed in `lakko-mls` and **still open in the
   product** — an owner has no way to reach the control that would let them leave. This is
   now the largest outstanding UX item in this spec, and it is a pass that has not happened
   rather than an implementation that skipped it.

   **And a second, smaller gap appeared alongside it: `Leave group` itself shipped on
   WEB ONLY.** §5's panel order puts it at the bottom of the members panel on all three
   platforms; web has it (hidden for the owner, as specified), iOS and Android built the
   rest of the membership surface without it. So the member-facing half of what
   [ADR-0045] unlocked — naming your own leaf in a Remove proposal, which needs no role —
   is reachable on one client of three.
4. **Visibility (§10).** I recommend owner/admin only, plus your own request. The
   counter-argument — that a member deserves to know a change is coming — is a values
   position, and it is the maintainer's.
5. **The unbounded re-request loop (§7).** With `MAX_ATTEMPTS` gone, a request that keeps
   being superseded is re-requested indefinitely and a partially-censoring committer now
   presents as a permanent `waiting` with no user-side signal at all. Recommended
   mitigation is in §7 (re-request only on an observed superseding commit, never on a
   timer) — enough to bound journal litter and battery, **not** enough to make censorship
   visible. Accept, or ask for a different treatment. **Moot for now and worth saying so:
   no client re-requests at all** (§7), so there is no loop to bound; a superseded request
   is dropped and the user asks again. The decision is still owed, but it is owed *before*
   the re-request is built rather than about something that exists.
6. **Typed refusal reasons on the FFI (§9.1).** Prose-matching `Error::Group` strings across
   three clients would work and would be wrong. Worth the small core change. **Still open,
   and the clients did the defensible thing rather than the wrong one:** none of them
   built the six-string refusal table this spec specifies, precisely because mapping core
   prose to fixed copy is the drift class the spec flags. So the table is unbuilt *and*
   the FFI change it depends on is unmade — the two go together and neither should ship
   without the other.
7. **Android does not refuse a second concurrent role change** (§5.3). Web and iOS do, and
   say so with the copy quoted there. The loss on Android is silent, which is the worst
   available shape for it. Whether the fix belongs on the third client or in
   `Group::propose_group_roles` — where it would hold for every caller and could not drift
   between platforms — is a real choice, at the cost of putting a client-ergonomics rule
   into the core.

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
  real remedies are ownership transfer (**now built** — `e780952`) and appointing more
  admins, both outside this spec. **Both remedies are pre-emptive, and that limit is worth
  stating**: transfer needs the *current* owner's key to mint, so it helps a group whose
  owner hands on deliberately and does nothing for one whose owner has already stopped
  running the app — which is the case that actually stalls. Removing `stalled` made this
  *more* true, not less — the earlier design
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
