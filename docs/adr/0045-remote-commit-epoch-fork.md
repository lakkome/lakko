<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0045: Closing the remote-commit epoch fork — proposal-based commits with a designated committer (G-38 residual 1)

> **Amended by [ADR-0081](0081-devicegroup-commits-cannot-strand.md)
> (2026-10-18).** The staged lifecycle this ADR built for the sweep
> (`has_staged_sweep`, `staged_export_secret`, `merge_staged_sweep`,
> `abandon_staged_sweep`) now also carries the direct commits of a group
> with no role table: `stage_add_members`, `stage_remove_member` and
> `stage_set_group_roles` leave the group at N holding the commit as
> pending, and the clients finish an unanswered one through the same owed
> verdict (`resolveOwedVerdict`). The verdict is the DS commit route's
> answer (`201` / `200` won, `409` lost) rather than a journal read alone.
> Where this document says only `stage_pending_proposals` can stage a
> commit, or that every direct path merges before it returns, read "every
> eager direct path".
>
> **Amended again by ADR-0081 (2026-10-19, "Amendment (2026-10-19,
> second)").** "A group with no role table keeps direct commits" no longer
> holds on any client: the direct paths are deleted, a membership operation
> on such a group is refused with `NoRoleTable`, and the core refuses a
> conversation Welcome without a role table, so no client holds one. The core
> keeps its staged twins, uncalled. Obligation 5 (every direct path becomes
> propose-or-sweep) is therefore complete.

- **Status:** **Accepted.** **The arc is built end to end** — the core (sprints
  S0-S3 plus four unplanned ones), both FFI surfaces, and **all three clients**
  (S4a/S4b web, S5 iOS, S6 Android); see the date line and the
  [blast-radius table](#blast-radius-per-sprint). Decision 2's **candidate
  heartbeat is live**, not merely present: every client emits a throttled beacon
  from a foregrounded chat screen, stores the observations, and folds them into
  an acting-committer verdict at sweep time, so an admin-only group no longer
  waits for an absent owner. Earlier revisions of this line said the mechanism was
  **inert**; that is no longer true and the passages that said so are corrected in
  place rather than deleted, because the reasoning under them still holds.
  **What is still open** is named honestly in
  [§ Open questions](#open-questions) and summarised here: **S7** — the
  external-join convergence of decision 4 — **landed in `576ea14`**, so no
  direct-commit path is left in a role-table group; the `lakko-security` pass
  **ran** (2026-08-29) and its seven findings are closed; the **external crypto
  review is tracked in [G-39](../security/known-gaps.md) per
  [ADR-0046](0046-external-crypto-review.md)** rather than as a blocker here, and
  it is a **release gate** — no build reaches users while it is open. Four of the UX
  spec's surfaces were listed here as unbuilt and **two of them have since shipped on
  all three clients**: the ownership-transfer flow (`0274fa7`) and the typed-refusal
  table (`8018e8b`), and with them `Leave group` — the member-facing feature this ADR
  unlocked — which reached the two natives in `c50bdd7`. **Two remain open**, and they
  are not the same kind: the **committer-side failure surface** is unbuilt *and
  unimplementable as specified* (§8 gates it on an activation delay belonging to the
  staggered variant the maintainer did not choose), and the **automatic re-request on
  supersession** is unbuilt *by a later decision* — a request the sweep dropped now
  reaches its author as the `Not applied (n)` surface (`46c7aa4`) with a user-driven
  repair, while the **sibling** case, a request refused because the queue was full, does
  re-send automatically on all three (`21f6c8c`, and `1839da7` for an approve).
  **The maintainer has chosen Option B — proposal-based commits.** This ADR was
  originally written as a recon that recommended Option C (client-side loser
  rollback); that recommendation has been **overturned by the maintainer** and
  this revision records the choice, states the reasoning that moved it, and
  settles the five design questions the recon left open. The recon of every
  option is kept intact below, because its value is the record of *why* the
  rejected options are rejected. Status stays **Proposed** until the maintainer
  accepts the settled design; the maintainer accepts, not the ADR.
- **Date:** 2026-08-18 (recon) · revised the same day to record the Option-B pick
  and settle its design · **2026-08-19: decision 2 (liveness) rewritten** to the
  model the maintainer chose — active admin devices elect the committer, the next
  candidate takes over when it is not acting, and a request waits indefinitely if
  none of them is running. That rewrite deletes the retry cap, the terminal
  *stalled* state and the `owner_grace` constant, and it **corrects a factual error
  in the previous revision**, which asserted that a conversation group has no
  presence signal. It has two (ADR-0024 typing and read receipts) and
  `lakko-presence` is a live exported surface; what is genuinely missing is a
  *continuous* beacon, which is a narrower and more tractable gap. Decision 1,
  decision 3, decision 4 and the whole recon are unchanged.
  · **2026-08-20: reconciled with the built core.** Four core sprints landed
  across three commits — `9a60ab3` (S1 the gate + S2 the committer), `c5c0788`
  (S3 both FFI surfaces, plus the S0 UX spec, written after the core rather than
  before it), `e780952` (**ownership transfer**, ADR-0022's deferred follow-up,
  and with it the commit-level `owner_survives` rule), and `fe3de6c` (the
  **staged-sweep rollback** — `stage_pending_proposals` / `merge_staged_sweep` /
  `abandon_staged_sweep` — together with the **unmute** fix for a defect the
  rollback work surfaced). Each of the last three found a consequence this ADR
  did not record, and every one of them is a **client** obligation, so they are
  written into the body here before S4-S6 are built against it:
  1. the rollback point is the **absence of the eager merge**, not a "retained
     epoch-N snapshot" — decision 2, layer 4;
  2. a staged sweep **freezes that group's own membership operations**, and the
     staged commit is **durable**, so a client that stages and never adjudicates
     wedges the group across restarts — decision 2, layer 4;
  3. a stored proposal **muted the whole group's chat** until the sweep, because
     OpenMLS refuses `create_message` while the proposal store is non-empty —
     decision 3;
  4. **by-reference resolution runs at commit receipt on every member**, not
     only at sweep time on the committer, which is what makes "store at receipt"
     mandatory rather than tidy — decision 3;
  5. ownership transfer is **built**, and the security content of that sprint was
     `owner_survives`, which closes a hole that predates it — decision 2 and
     [§ What Option B does not fix](#what-option-b-does-not-fix) item 2.

  This revision also mints the ADR's first indexed invariant, **C-63**,
  reversing this ADR's earlier claim that it proposed none
  (see [§ Invariants touched](#invariants-touched)).
  · **2026-08-24: reconciled with two further sprints.** **S3b** (`6c77cd6`) —
  the FFI back-fill for the staged sweep and ownership transfer, which this ADR
  had recorded as missing; that claim is now marked corrected rather than deleted.
  **S3c** (`94825ec` core, `94da492` both FFI surfaces + the slim NSE surface,
  `5d6af94` + `f53d11a` six client filter arms) — decision 2's **candidate
  heartbeat**, the variant the maintainer chose in `3f09ca1`. It lands a field-less
  27-byte beacon envelope in `lakko-contacts`, the pure `acting_committer_verdict`
  fold, the `should_emit_beacon` throttle, four constants, a fuzz target, and
  `Group::committerRank` for ranking a beacon's MLS-authenticated sender. **No
  `lakko-mls` change — invariant #3 is untouched by this sprint.** Three things it
  settled that this ADR could only pose are now in the body: **why the payload
  carries no fields** (a self-reported timestamp would let a candidate hold the
  committer duty by asserting its own freshness — a liveness claim no peer can
  check); **why `lakko-presence` is the wrong primitive** despite being the obvious
  candidate; and **why the interval and window are chosen rather than measured**,
  with the budget argument that stands in for the measurement this ADR asked for.
  One consequence it did not anticipate: a beacon collides with `parse_body`'s
  lossy-UTF-8 legacy fallback, so the sprint had to add a service-envelope filter
  to six client receive paths before a beacon could ride the journal without
  appearing as a chat bubble. **The emitter and the observation store are not part
  of it and remain the largest S4-S6 item.**
  · **2026-08-25: reconciled with the three client sprints.** **S4a** (`1de5fe7`) —
  the web reference: admins propose, the owner sweeps, with the five obligations
  each placed somewhere a test can run it. **S4b** (`02f0037`) — the beacon emitter,
  the observation store and the fold, which ends the admin-only group's wait and
  gives `composeCommitterBeacon` its first call site anywhere in the tree.
  **`000d9f8`** — the send-path serialization, an unplanned fix for a hazard the
  staged sweep turns from a race into a certainty. **S5 + S6** (`1f230ab`) — the iOS
  and Android mirrors, which satisfy the beacon's worst-case-poll constraint that
  web structurally does not. Five things the clients found that this ADR did not
  anticipate are written into the body here rather than left in commit messages —
  see [§ What the client sprints found](#what-the-client-sprints-found). This
  revision also **mints C-64**, the indexed invariant this ADR deferred until "the
  first client that emits a beacon and folds a verdict"; that condition is met, so
  the deferral section is rewritten into the mint, scoped as it always said it
  would have to be — to the election and the sweep, not to the beacon alone.
- **Context:** Phase 3 — group-membership commits / MLS epoch integrity. Closes
  **G-38 residual 1**, the largest unfixed item on the client-concurrency list
  and the only one on it that **no client-side lock can address by
  construction**, because the second committer is a different device.
- **Relationship to prior ADRs.** **Extends
  [ADR-0040](0040-durable-pending-commit-outbox.md); supersedes nothing.** The
  durable pending-commit outbox, the fail-closed step-2 atomic write, and the
  idempotent restart sweep all carry through verbatim. ~~The eager merge inside
  the create-commit call~~ carries through **on the four direct-commit paths
  only**: the new sweep path stages instead of merging, which is ADR-0040's own
  deferred "Option B" scoped narrowly — see
  [§ The staged sweep, as built](#the-staged-sweep-as-built).
  ADR-0040 closed the **crash** window between publish and
  persist; [ADR-0044](0044-push-routing-secret-recency.md)'s serializing per-group
  lock closed the **local** two-in-flight window (G-38 proper). This ADR addresses
  the third route to a divergent epoch, the one **both** of those leave open.

**Anchor convention.** Line numbers are as of the working tree this ADR was
written against (`a751335` plus the uncommitted G-37 follow-ups). Symbol names
are the durable reference — ADR-0043 and ADR-0044 both recorded that these files
drift by hundreds of lines within a single arc.

---

## Context

### The hazard, stated so the mechanism is not mistaken for G-38 proper

G-38 proper was **one device** with **two commits in flight**, and it was fixed by
a per-group serializing lock spanning create-through-publish on all three
clients. This residual is a different shape and the lock is powerless against it:

> Device **A** and device **B** are both members of group G at epoch **N**. Each
> independently builds a membership commit against N — A adds Alice, B removes
> Bob. Each eager-merges to N+1 locally *inside the create call*
> (`crates/lakko-mls/src/group.rs`, `add_member` / `remove_member` /
> `set_group_context_extensions` / `approve_external_join`, per ADR-0040's table),
> durably records the outbox entry + the N+1 snapshot, and publishes. The DS
> journal orders by **arrival** (`ds_group_messages.id AUTO_INCREMENT`,
> `services/ds/migrations/0001_init.sql`). Exactly one of them lands first.

Both commits are framed at epoch **N**. That is not incidental — it is RFC 9420's
rule and openmls enforces it as ValSem003
(`openmls-0.8.1/src/group/public_group/validation.rs:56-80`):

```rust
// For application messages we allow messages for older epochs as well
ContentType::Application => { if message.epoch() > … { WrongEpoch } }
// For all other messages we only only accept the current epoch
_ => { if message.epoch() != self.group_context().epoch() { WrongEpoch } }
```

So a Commit carries the epoch it was **built against** (N) and applying it
advances the applier to N+1.

**What actually happens.** Say A's commit `C_A` lands first.

- Every other member applies `C_A`, advancing N → N+1.
- `C_B` arrives after. Everyone is now at N+1; `C_B` is framed at N; ValSem003
  rejects it as `WrongEpoch`. **They drop it and never see B's removal of Bob.**
- **B is already at N+1 — its own N+1, produced by `C_B`.** B then polls, receives
  `C_A` framed at N, and *its* ValSem003 rejects it the same way. B drops it.

Both sides now report **epoch N+1** and hold **different group state**: different
ratchet trees, different tree hash, different epoch secrets, different roster. The
epoch *number* matches, which is precisely why nothing anywhere raises an alarm.

### It is silent, and it is permanent

The receive loop treats every apply failure identically — log and skip, cursor
advances (`apps/web/src/chat/client.ts:730-745`):

```ts
} catch (err) {
  // Bad frame, wrong epoch, malformed wire, OR a role-unauthorized
  // commit refused by the gate — log and skip. Cursor still advances.
  console.warn("processIncomingVerifyingRoles failed", message.id, err);
  continue;
}
```

The iOS and Android poll loops carry the same catch-and-continue shape (they had
to, for ADR-0040's idempotent duplicate re-publish to be safe). So:

- **No error reaches the user.** Not on B, not on any peer.
- **There is no recovery path.** The group's *next* commit will be framed at N+1,
  which equals B's own epoch — so B's ValSem003 **passes** and the failure moves
  one layer down to AEAD, surfacing as a decryption failure rather than an epoch
  mismatch. Either way it is the same `console.warn` and the same `continue`. B
  never re-converges.
- **Traffic breaks in both directions.** B's application messages are framed at
  N+1 and sealed under B's epoch secrets; peers at N+1 cannot open them. Peers'
  messages are equally opaque to B.

The user-visible symptom is "my messages stopped arriving in this group", with no
diagnostic anywhere, and the only repair available today is out-of-band: another
admin removes B and re-adds it, or B rejoins from a fresh Welcome.

**Severity: high. Frequency: unknown, and this ADR does not invent a number.**
What can be said structurally is that the plausible collisions are *correlated*,
not independent — the realistic shapes are two admins reacting to the same event:

- two admins both removing the same spammer they both just saw;
- two admins both approving the **same** pending join request, which ADR-0027's
  members-panel pending list surfaces to every owner/admin simultaneously and
  ADR-0028's invite-link flow is explicitly designed to produce;
- an ADR-0043 removal arc, where the removal commit is followed by a rotation
  broadcast, widening the window a second admin can land inside.

Correlated triggers are exactly the case where "rare" reasoning fails.

### Why no client-side lock helps

ADR-0044's C-62 lesson — a read-decide-write must run under one serializing
per-`(account, group)` lock — is about a **single** process's view of shared local
state. Here the two writers are two devices with no shared memory, no shared
store, and no shared lock namespace. The only thing they share is the DS journal,
and the DS assigns order **after** both have already eager-merged. A lock cannot
reach across that boundary; only a shared arbiter (the journal itself, or the
server, or a designated committer) can.

### What ADR-0040 provides, and precisely where it stops

The outbox makes a commit **durable before publish** so a crash can re-publish it.
That is the right primitive for the crash window and the wrong shape for this one:

- **Both publishes succeed.** Both outbox entries get acked; the restart sweep has
  nothing to re-publish. ADR-0040's machinery walks straight past this failure.
- **The rollback path exists but is structurally unusable here.**
  `resetGroupToDurableSnapshotOrThrow`
  (`apps/android/app/src/main/kotlin/me/lakko/app/chat/ChatRepository.kt:2786`,
  and its web/iOS twins) restores the provider from the durable snapshot to drop
  an unpublished eager merge. Its correctness argument is stated in its own
  doc-comment: *"the durable snapshot is still at N (the commit's persist
  failed)"*. In this hazard the ADR-0040 step-2 atomic write **succeeded**, so the
  durable snapshot **is N+1**. Restoring from it is a no-op. The existing abort
  path cannot roll B back.
- **The outbox holds no pre-commit state.** `PendingCommitEntry`
  (`apps/web/src/chat/commit-outbox.ts`) carries `targetEpoch`, `commitB64`, the
  second artifact, and `acked` — the *forward* artifacts. Nothing retains epoch N.

**So any repair for this hazard needs a durable artifact that does not exist
today on any client: the pre-commit (epoch N) snapshot, retained until the commit
is known to have won.** That fact is what actually sizes the options below, and it
is why the "cheapest by far" option is not free.

### Recon: what the DS is today

Verified in code, because the epoch-gate option turns entirely on it.

- **The DS parses nothing.** `services/ds/src/main.rs:16-24`: *"Payloads are
  opaque MLS ciphertext. The server does not parse them, does not interpret epoch
  / seq / sender — just stores the bytes and assigns a per-row monotonic id."*
  `services/ds/src/journal.rs:1-16` repeats it. The `publish` handler
  (`journal.rs:177`) collects the body and inserts it; there is no branch on
  content.
- **The DS holds no per-group state.** `services/ds/migrations/0001_init.sql` has
  exactly two tables, both `(id, owner_id, payload, created_at)`. There is no
  per-group row an epoch counter could live in; a gate needs a **new table and a
  new write on the publish hot path**.
- **The DS has no authentication.** `services/ds/src/main.rs:26-29`: *"No
  authentication on Phase 1.3 — anyone with the right pseudonym can publish or
  fetch. Pseudonyms are 32 bytes random; guessing one is infeasible."* The whole
  access-control model is "the pseudonym is a secret".
- **The pseudonym IS the MLS `group_id`.** `apps/web/src/ds/client.ts:171` builds
  `/ds/group/${pseudonymHex(groupId)}/messages`, and `pseudonymHex`
  (`ds/client.ts:45`) is plain hex — no derivation, no blinding. This is the same
  fact ADR-0042 recorded when it moved push routing off it.
- **The `group_id` is deliberately handed to non-members.** A
  `GroupInviteCapability` (`crates/lakko-contacts/src/group_invite.rs:41,81`)
  carries *"[gid_len] group_id — opaque MLS GroupId bytes"* in the clear inside a
  `/g/<slug>` link, because ADR-0028's whole design is that a joiner reads the
  group's epoch from the journal before it is a member. **Anyone who has ever held
  a group invite link can publish to that group's journal.** Today that is
  harmless: clients drop the garbage. It stops being harmless the moment the
  server acts on what is published.
- **The journal carries three payload classes, not one.** Verified at the web
  publish sites:
  - raw MLS messages — commits (`contacts/client.ts:1822`, `:1931`, `:2035`,
    `:2126`) and application messages (`chat/client.ts:483`, `:617`);
  - raw MLS **external-join proposals** from non-members
    (`contacts/client.ts:2381`) — `PublicMessage`, parseable, framed at N;
  - **non-MLS Lakko envelopes**: the ADR-0027 `WelcomeRendezvous`
    (`contacts/client.ts:2134`) and the ADR-0028 OQ2 join attribution
    (`contacts/client.ts:2395`). Neither parses as `MlsMessageIn`.
  - and, on the **same endpoint and the same table**, **every DeviceGroup message
    is wrapped**: the ADR-0015 `version‖attestation_len‖attestation‖mls_message`
    envelope from `encodeJournalEntry` is applied at every DeviceGroup
    `publishGroup` site (`apps/web/src/devices/sync/DeviceGroupSyncContext.tsx:464`,
    `devices/remove.ts:165`, `devices/attested-removal-policy.ts:49`,
    `devices/orchestrator.ts:588`, plus the iOS/Android twins). There is no
    raw-MLS fallback (`apps/web/src/devices/journal-envelope.ts:29-30`).
    **A DeviceGroup commit is not parseable as an MLS message from the outside** —
    though it *contains* one, at offset `5 + attestation_len`.
- **The three non-MLS envelopes fail the MLS parse fast and unambiguously.** An
  `MLSMessage` opens with `ProtocolVersion` as a `u16` = `0x0001`; the rendezvous
  and DeviceGroup envelopes open `0x01 0x00…` and the attribution envelope opens
  `0x02 …`, so `tls_deserialize` rejects all three immediately. A DS-side peek can
  therefore be `Option`-shaped exactly like `epoch_of_message` — **but it must
  never 4xx on a failed parse**, or it breaks ADR-0027 approvals, ADR-0028 OQ2/OQ3,
  and every DeviceGroup operation.
- **A per-row epoch is not a per-group epoch.** One journal legitimately carries
  rows at several epochs at once: an ADR-0040 idempotent outbox **re-publish** is a
  Commit at a now-stale epoch (`contacts/client.ts:611`), and a non-member's
  external-join proposal is crafted against whatever epoch it read from the head
  and may already be stale (`contacts/client.ts:2381`). Any gate has to be right
  about both.

### Recon: can the DS read a commit's epoch pre-decrypt? **Yes — verified.**

This was the load-bearing question and the answer is unambiguous.

`lakko_mls::epoch_of_message` (`crates/lakko-mls/src/group.rs:1009`) is a
free function taking `&[u8]` and returning `Option<u64>`, with no `Identity`, no
provider, no group state:

```rust
pub fn epoch_of_message(bytes: &[u8]) -> Option<u64> {
    let mut slice = bytes;
    let msg = MlsMessageIn::tls_deserialize(&mut slice).ok()?;
    let proto = msg.try_into_protocol_message().ok()?;
    Some(proto.epoch().as_u64())
}
```

`ProtocolMessage::epoch()` (`openmls-0.8.1/src/framing/message_in.rs:203-209`)
dispatches to either variant, and `PrivateMessageIn`
(`openmls-0.8.1/src/framing/private_message_in.rs:39-46`) declares
`group_id`, `epoch`, and `content_type` as **plain deserialized struct fields**
sitting ahead of `encrypted_sender_data` and `ciphertext`. They are cleartext on
the wire by RFC 9420's framing design — the DS already receives them today and
already stores them.

**It applies to Commits as well as Application messages.** ADR-0028's existing
test (`crates/lakko-mls/src/group.rs:1241`,
`epoch_of_message_reads_public_framing`) only exercises application messages, so
that specific assertion is not currently pinned in the test suite — but the code
path is variant-agnostic (`try_into_protocol_message` accepts both wire formats,
`epoch()` reads the same field) and openmls's own ValSem003 reads `epoch()` and
`content_type()` off the same pre-decrypt `ProtocolMessage` to *classify commits*.

**More than the epoch is available.**
`ProtocolMessage::content_type()` (`message_in.rs:211-217`) and
`is_handshake_message()` (`:233-236`) are equally pre-decrypt, so the DS could
distinguish a **Commit** from an Application message or a Proposal without any
group secret. A gate can therefore be narrow — act on Commits only, pass
everything else through untouched.

**What the DS would have to parse, concretely:** enough of the RFC 9420
`MlsMessage` TLS framing to reach `group_id`, `epoch` and `content_type` — i.e. it
would take a dependency on `lakko-mls` (and transitively openmls) or on a new
minimal framing-peek crate, and it would need a new `ds_group_epochs (group_id,
current_epoch)` table written under a per-group lock on the publish path.

**And the information-disclosure question has a clean answer: none.** Every byte a
gate would read is already in the row the DS stores. Nothing new is revealed.
The cost of Option A is **not** disclosure. It is entirely about **behaviour**.

---

## The load-bearing framing

State this before the options, because it collapses most of the comparison:

> **Detection and repair are separate problems, and every option needs the
> repair.** Learning that you lost the race is cheap. What is expensive is
> rolling a device back off an epoch it has already eager-merged and durably
> persisted, and then re-expressing the user's *intent* against the epoch that
> actually won.

A DS epoch gate is a **detection** mechanism. It returns a synchronous rejection
at publish time instead of an asynchronous inference from the journal. It does
**not** roll B back, does **not** re-apply `C_A`, and does **not** retry the
removal of Bob. B still needs the retained epoch-N snapshot, the replay, and the
bounded retry — the entire Option C machinery — before the gate's rejection means
anything but a different flavour of stuck.

**So Option A is not an alternative to Option C. It is additional cost on top of
it, buying an earlier and slightly cleaner verdict.** That reframing is the single
most useful output of this recon, and it is what the *original* recommendation
rested on.

Option B is the near-exception, and the first draft of this ADR **overstated it**.
It said B "removes the race rather than repairing its loser, so it genuinely does
not need the rollback." That is wrong, and it contradicts B's own cost item 2 four
paragraphs later. **Corrected:** B removes the race in the steady state — one
committer, so there is nobody to race — but it does not remove it during a
committer handoff, where two devices can both act. What B changes, and this is the
real result, is **which artifact loses**:

> Under today's design and under Option C, the artifact that loses a race is a
> **Commit**, and losing it forks the loser off the group — expensive,
> silent, and needing a durable epoch-N snapshot plus an intent replay to repair.
> Under Option B the artifact that loses is a **Proposal**, and losing it costs
> **nothing**: a proposal is not epoch-advancing, so its author is not forked, has
> no rollback to perform, and re-proposes against the new epoch as a plain retry.

B therefore still needs a loser-repair path, but only for the committer, only
during handoff, and only its cheap half — see
[§ The Option B design, settled](#the-option-b-design-settled), decision 2. The
framing that survives is the sentence above: **B moves the retry from the
expensive artifact to the cheap one.** That is the whole of its advantage, and it
is enough.

---

## The options

### Option A — a DS-side epoch gate

**Mechanism.** On `POST /ds/group/{gid}/messages`, peek the payload. If it parses
as an MLS message with `content_type == Commit`, compare its framing epoch to a
per-group `current_epoch` the DS maintains; accept only `epoch == current_epoch`
and bump the counter in the same transaction; reject anything else with a 409.
Everything that does not parse as an MLS Commit passes through untouched.

**What it fixes.** `C_B` is refused at the door. B gets a structured, synchronous
"you lost" instead of silently forking. The window shrinks from "until the peers'
next commit" to "one publish round-trip".

**What it does not fix.**

1. **It does not repair B.** B is still durably at N+1 with a commit nobody will
   ever apply. Without the retained epoch-N snapshot and the replay, a 409 leaves
   B *exactly as forked as before*, just with a log line. The whole of Option C is
   still required.
2. **It does not cover DeviceGroups.** Every DeviceGroup commit is wrapped in the
   ADR-0015 journal envelope and will not parse as MLS, so it passes ungated. The
   gate would cover chat groups and channel PublisherGroups and silently skip the
   DeviceGroup — unless the DS *also* learns the ADR-0015 framing, which is a
   second Lakko-specific parser in the zero-trust tier.
3. **It does not survive a multi-instance DS.** The counter is a per-group
   read-modify-write on the publish hot path; today's DS has no cross-instance
   coordination at all (its long-poll fanout is explicitly in-process only,
   `main.rs:80-85`). The gate would become the first piece of DS state that cannot
   be horizontally scaled without a shared lock.
4. **It breaks ADR-0040's restart sweep, and the repair is Option C again.** The
   sweep re-publishes an un-acked commit unconditionally, and its correctness rests
   on the re-publish *always being accepted* and then being dropped by peers as
   wrong-epoch. Under a gate, a re-publish of a commit that already landed (the
   crash-after-publish-before-ack case) is framed at N against a counter already at
   N+1 and gets a **409**. The sweep cannot tell that 409 apart from "you lost the
   race" — both are "your commit was refused at epoch N" — so it would have to
   inspect the journal to distinguish them, which is precisely Option C's verdict
   step. **The gate does not merely fail to replace Option C; it makes Option C
   mandatory in a path where today none of it is needed.**

**The decisive cost: it creates a new permanent group-freeze primitive.**

The DS cannot verify a commit. It has no group secrets, so it cannot check the
confirmation tag, the membership tag, or the signature. It can only check that
the bytes *parse* and that the epoch field *matches*. Therefore:

> Anyone who can publish to the journal can post a **well-formed but
> cryptographically bogus** `PrivateMessage` with `content_type = Commit` and
> `epoch = N`. The DS accepts it and bumps `current_epoch` to N+1. Every real
> member drops it (bad confirmation tag) and stays at N. **Every subsequent honest
> commit is framed at N and is now refused forever**, and nobody can produce a
> commit at N+1 because no member is at N+1. The group's membership is frozen
> permanently — no adds, no removals, no role changes, no join approvals.

And "anyone who can publish" is a genuinely open set, because the DS has no
authentication and the pseudonym is the `group_id`: every member, the server
itself, and **anyone who has ever seen a `/g/` invite link**, including one that
has expired (the DS does not check capabilities; expiry is a client-side check
inside `decode_and_verify`).

Today that same attacker can post the same bytes and it costs nothing — clients
drop them. The gate converts a harmless nuisance into a **permanent group-wide
denial of service reachable by a stranger with a stale link**. Recovery requires
operator intervention on the DS.

Mitigations are all unsatisfying: authenticating DS publishes is a large separate
arc (and the natural authenticator — group membership — is exactly what the
zero-trust DS must not learn); a "follow the highest epoch seen" variant freezes
just as hard on a bogus high epoch; a PoW gate raises the cost of the freeze
without bounding it, since the freeze is permanent and one shot suffices.

**Where it stands relative to invariant #2.** Honestly: **inside the letter,
outside the spirit, and outside the DS's stated posture.** Nothing about
confidentiality changes — the epoch and content type are already on the wire and
already in the row. But invariant #2's companion property throughout this codebase
is that the server is *transport-only*, stated in the DS's own module docs
(*"does not interpret epoch / seq / sender"*) and in the threat model's assumption
of a **fully compromised server** whose only powers are read, drop, reorder, and
serve arbitrary bytes back. A gate adds a fifth power — **selective refusal on a
protocol-semantic predicate** — and makes the server a participant whose failure
modes now have to be reasoned about in the MLS layer. That is a real posture
change and should be named as one, not waved through on the "it's already visible"
argument.

*What a malicious DS gains* is, admittedly, thin: it can already drop `C_B`
silently and produce the identical fork. So a gate does not much expand the
*server's* powers. **The expansion is to everyone else** — the freeze primitive
above is available to parties who today have none, and that is the finding.

**Blast radius:** `services/ds` (new dependency on MLS framing, new table, new
migration, new per-group lock on the publish path, a new 409 the clients must
handle), plus the full Option C machinery on all three clients. Plus a new
availability attack surface. `lakko-security` mandatory.

### Option B — proposal-based commits — **CHOSEN**

> **This is the option the maintainer picked.** The recon below is kept as
> written, including the cost list that argued against it, because a future
> reader should see what the objections were and how they were answered. Two of
> them turned out to be smaller than stated (cost 1 and the ADR-0027 note) and
> one turned out to be larger (cost 2, the liveness single point). All five are
> resolved in [§ The Option B design, settled](#the-option-b-design-settled).

**Mechanism.** Members do not commit. A member emits an MLS **Proposal**
(`Add` / `Remove` / `GroupContextExtensions`); a single **designated committer**
collects pending proposals and issues the one commit. Concurrent intents become
concurrent *proposals*, which are not epoch-advancing and compose, so there is no
race to lose.

**openmls supports the shape.** Verified:
`propose_add_member` (`proposal.rs:270`), `propose_remove_member` (`:309`),
`propose_group_context_extensions` (`:395`), `store_pending_proposal`
(`processing.rs:206`), `commit_to_pending_proposals` (`:234`),
`clear_pending_proposals` (`mod.rs:397`). The `CommitBuilder`
(`commit_builder.rs:245-267`) offers the same as a batch.

**Lakko already has the committer-election precedent.** ADR-0032's
`lakko_calls::link_invite::elect_committer` (`crates/lakko-calls/src/link_invite.rs:230`)
picks *owner-if-present, else lowest-leaf present member*, recomputed on every
roster change, precisely so failover and reclaim are automatic and split-brain-free.
ADR-0022's role table gives chat groups a canonical owner to elect.

**What it fixes.** The race, at the root. No detection, no rollback, no retry, no
server change, no new attack surface. It is the correct-by-construction answer.

**What it costs — and this is larger than it first looks.**

1. **It moves the ADR-0022 authorisation surface, and today that surface does not
   exist.** The role gate authorises the **committer**:
   `process_incoming_verifying_roles` (`crates/lakko-mls/src/group.rs:582-632`)
   reads the committer leaf from `processed.sender()` and runs
   `authorise_role_commit` against the pre-merge role table. Look at what it does
   with a proposal (`:627`):

   ```rust
   ProcessedMessageContent::ProposalMessage(_) => Ok(Incoming::Proposal),
   ```

   The proposal is **discarded, unexamined, un-role-checked** — safe today only
   because Lakko never stores member proposals, so they can never be swept into a
   commit. Under Option B they must be stored, and `commit_to_pending_proposals`
   commits **everything pending**. A plain member could inject an `Add` proposal
   that an honest owner's commit then carries. **The role gate would have to move
   from the committer to the proposal sender, and the committer would additionally
   have to refuse to sweep unauthorised proposals.** That is a new authorisation
   rule in the audited MLS state machine — invariant #3, requiring an ADR,
   property tests, and external review. It is also exactly the kind of change
   where a subtle miss re-opens ADR-0022's threat model.

   ADR-0027 already hit the neighbouring edge and chose to avoid it: it rejected
   `commit_to_pending_proposals` in favour of the single-target `add_member`
   precisely because commit-all does not let the approver target one joiner.

   > **Correction, found while settling the design.** ADR-0027's stated reason no
   > longer holds, and the "commits everything pending" premise of this whole cost
   > item is only true of the *convenience wrapper*. `commit_to_pending_proposals`
   > is a thin shim over the `CommitBuilder` that hardcodes the filter to
   > `|_| true` (`openmls-0.8.1/src/group/mls_group/processing.rs:246-253`), and
   > the builder's own entry point is
   > `CommitBuilder::build(rand, crypto, signer, f: impl FnMut(&QueuedProposal) -> bool)`
   > (`commit_builder.rs:420-428`) — openmls's documented hook for exactly this,
   > *"a function that lets the caller filter the proposals that are considered
   > for inclusion … a way for the application to enforce custom policies in the
   > creation of commits"* (`:430-433`). So the committer-side sweep gate is a
   > closure over an API openmls already publishes, **not** an openmls fork and
   > **not** a new commit path. This makes cost item 1 materially smaller than
   > stated, and it means the ADR-0027 approve path and the member-proposal path
   > can converge on one mechanism rather than staying separate.

2. **It introduces a liveness single point.** No committer online, no membership
   change. `elect_committer` mitigates this, but ADR-0032 recorded the residual
   candidly: for a weakly-authenticated participant set, "which committer admits
   me" is inherently weakly authenticated. During a handover window two devices
   can both believe they are committer — which reproduces this very race, in a
   narrower window.

3. **It is a UX change on three clients.** "Remove Bob" stops being immediate and
   becomes "removal requested, pending". Empty/pending/failed/timeout states,
   pending-proposal lists, cancel affordances. Per the repo's UX-first rule this
   needs `ux-ui` **before** the core work, not after.

4. **It interacts with everything downstream of a commit.** ADR-0040's outbox
   (the proposal is not epoch-advancing, so it needs no outbox — but the
   committer's commit still does), ADR-0043's C-61 removal-then-rotation ordering
   (the rotation is triggered by observing a membership shrink, which now happens
   at commit time on a device that did not request it), ADR-0027/0028's
   approve path (which fits *better* — an external join is already a proposal),
   and every membership-panel surface on three clients.

**Blast radius:** `crates/lakko-mls` (invariant #3), both FFI surfaces, all three
clients' membership UX and repositories, ADR-0022's gate semantics, ADR-0043's
rotation trigger. The largest option by a wide margin.

### Option C — client-side loser detection, rollback, and bounded retry

**Mechanism.** No server change, no core change, no wire change.

1. **Retain the pre-commit snapshot.** Extend ADR-0040's step-2 atomic write to
   also durably keep the **epoch-N** provider snapshot, alongside the N+1 snapshot
   and the outbox entry, in the same all-or-nothing write (web: the existing
   `transaction([snapshots, commit-outbox])`; iOS/Android: the existing
   entry-first-then-snapshot ordering with the sweep's discard rule extended). This
   is the artifact nothing on any client has today.
2. **Publish the commit. Do not publish the Welcome yet** — see the ordering
   constraint below.
3. **Obtain the verdict.** Fetch the journal from the pre-publish cursor through
   the returned `messageId M`. If any row with `id < M` is an MLS message whose
   framing epoch equals N and whose content type is Commit, **we lost**. Otherwise
   we won.
4. **On a win:** publish the second artifact (Welcome / rendezvous), ack the
   outbox entry, drop the retained N snapshot. Steady state, byte-identical to
   today plus one retained blob and one extra fetch.
5. **On a loss:** restore the provider from the retained N snapshot, replay the
   journal in order (which applies the winner and lands at the group's real N+1),
   discard the outbox entry, and **retry the original intent** — not the original
   bytes — as a fresh commit against the new epoch, under a bounded retry budget.
   Exhausting the budget surfaces a real, user-visible "could not apply your
   change" instead of a silent fork.
6. **The restart sweep** re-publishes an un-acked entry as today, then runs the
   same verdict step. Idempotent.

**Why the verdict is reliable.** The DS returns rows in `id` order and the client
knows its own `messageId`; a foreign epoch-N commit at a lower id is a total,
deterministic ordering fact. A malicious DS could serve B a journal view that
hides `C_A` — but a malicious DS can already drop `C_A` outright and produce the
identical fork, so detection is no weaker than today against that adversary. It is
exactly correct against the honest-but-racy DS, which is the actual hazard.

**A load-bearing ordering constraint this creates.** In the add and approve paths
ADR-0040 publishes the commit **and then** the `WelcomeRendezvous`
(`contacts/client.ts:2126` then `:2134`). If both are published and the commit
then loses, the joiner may already have opened a Welcome into a group state that
**no other member holds** — a phantom fork with a third party in it, strictly
worse than the two-device case. So under Option C the Welcome must be published
**only after the verdict**. This composes cleanly with ADR-0040: both artifacts
are still *durably recorded* before any publish (the outbox guarantee is
untouched); only the *publication* of the second is deferred past the verdict.
Recording this constraint is arguably the most important single line in this ADR,
because getting it wrong turns a repairable two-party fork into an unrepairable
three-party one.

**Honest list of what Option C does not fix.**

- **It repairs, it does not prevent.** The loser's dead commit stays in the
  journal forever (30-day TTL) and every peer spends one failed apply on it. Cheap
  and bounded, but it is litter.
- **The add-path retry may need a fresh KeyPackage.** `add_member` *"Consumes one
  of the new member's published KeyPackages on success"*
  (`crates/lakko-mls/src/group.rs`, `add_member` doc). A discarded commit may have
  consumed one; the retry may have to re-fetch from discovery, and a joiner with
  no spare KeyPackages published would fail the retry. Needs a concrete answer.
- **The approve-path retry needs the joiner's proposal at the new epoch.**
  ADR-0027's pending external joins are epoch-bound and held **in memory**; after
  the replay the retained proposal is stale. ADR-0028's joiner already retries on
  a stale epoch, so the recovery exists — but the approve retry is not
  self-contained, it waits on the joiner. Needs a concrete answer.
- **The rollback is whole-provider.** `restoreStorage` replaces every group's
  state; there is no per-group restore in the FFI. This is the pre-existing
  limitation G-37's residual and G-38 residual 4 both name, and Android's
  `providerLock` doc-comment
  (`ChatRepository.kt:215-238`) already states the discipline that keeps it safe —
  every provider-touching region takes the lock, so other groups are provably
  in-memory-equals-durable whenever a restore runs. **So Option C's rollback is no
  worse than the abort path already shipped**, which is a genuinely useful result:
  it needs no new safety argument, only the existing one.
- **N-way races degrade gracefully, not deterministically.** Three concurrent
  committers: two lose, both retry, and they can collide again. The retry budget
  bounds the livelock; it does not eliminate it.
- **It costs one extra journal fetch per membership commit.** Small, but on the
  interactive path.

**Blast radius:** three clients, entirely in the existing ADR-0040 seams
(`contacts/client.ts` + `chat/commit-outbox.ts` + `mls-store.ts`;
`ChatRepository.swift` + `CommitOutbox.swift`; `ChatRepository.kt` +
`CommitOutbox.kt`). No Rust, no FFI, no wire, no server, no schema, no new attack
surface. `lakko-security` warranted (it touches the commit ordering and the
ADR-0040 durability argument) but the change is orchestration over already-audited
primitives.

### Option C′ — the cheap companion: narrow the window

Independently of the above, re-read the journal head immediately before creating
the commit and abort if it moved. This does **not** close anything — the window
shrinks from "create + publish" to "publish RTT" — but it is a handful of lines,
it composes with every other option, and it converts the common case (a second
admin acting seconds later, not milliseconds later) into a clean no-op. Worth
doing regardless of which option wins. It must **not** be shipped alone and
described as a fix.

### Option D — do nothing

**What it costs.** A silent, permanent, unrecoverable-without-out-of-band-help
fork of one device from one group, on a low-frequency but *correlated* trigger,
with no diagnostic anywhere in the system. The user experience is "this group
stopped working for me" and the support answer is "have an admin remove and re-add
you".

**Rejected.** Not because of the frequency — which this ADR declines to guess —
but because of the shape: silent plus permanent plus no diagnostic is the
combination that makes a rare bug expensive. The Option C′ mitigation alone would
already be an improvement over this, and Option C is not a large change.

---

## The recon's original recommendation — **overturned, kept for the record**

> The recon recommended **Option C**. The maintainer chose **Option B**. This
> section is preserved verbatim because reasons 1-4 are the case against Option A
> and remain in force; reasons 5 and 6 are the case for C over B and are the ones
> the maintainer's correction answers. See
> [§ Why the choice moved to Option B](#why-the-choice-moved-to-option-b).

**Recommend Option C, plus Option C′ as a companion. Do not build Option A. Keep
Option B on file as the eventual correct-by-construction shape.**

The reasoning, in order of weight:

1. **Option A does not replace Option C; it adds to it.** The gate produces a
   verdict; it does not roll the loser back, does not replay, does not retry. All
   of Option C's machinery is still required underneath. Choosing A means building
   C *and* a server change *and* accepting a new attack surface, to move the
   verdict from "one extra fetch" to "the publish response".
2. **Option A hands a permanent group-freeze primitive to anyone holding a stale
   `/g/` link.** The DS has no auth, the pseudonym is the `group_id`, and ADR-0028
   deliberately puts the `group_id` in a shareable link. The DS cannot verify a
   commit, so it must accept well-formed garbage and advance its counter on it.
   That is a new, permanent, operator-intervention-only denial of service where
   today the same bytes cost nothing. Trading a rare silent fork for a cheap
   permanent freeze is a bad trade.
3. **Option A would also be a partial fix**, silently skipping DeviceGroup commits
   because they are wrapped in the ADR-0015 envelope and do not parse as MLS — and
   it would *break* ADR-0040's restart sweep, whose 409 is indistinguishable from a
   lost race without the journal inspection Option C provides.
4. **Option A changes the DS's posture.** Not invariant #2's confidentiality
   letter — the epoch and content type are already on the wire — but the
   transport-only property the DS's own module docs assert and the threat model
   assumes. That deserves a deliberate decision, not a side effect.
5. **Option B is right and too big for this.** It is an invariant-#3 change that
   relocates ADR-0022's authorisation surface to a place where no gate currently
   exists (`ProposalMessage(_) => Ok(Incoming::Proposal)` — discarded, unchecked),
   introduces a liveness single point, and changes the membership UX on three
   clients. It is the correct answer for a product with heavy multi-admin
   concurrency. It is not proportionate to this hazard today.
6. **Option C is client-only and reuses what already exists.** The rollback is the
   already-shipped whole-provider restore with the already-argued `providerLock`
   safety property; the durable write is the already-atomic ADR-0040 step 2 with
   one more blob; the detection is one fetch against a total order the DS already
   provides. No Rust, no FFI, no wire, no server, no new attack surface. It
   converts a silent permanent fork into an automatic bounded retry, and when the
   retry budget is exhausted it converts it into a **visible error**, which is
   most of the value on its own.

**The honest limitation of the recommendation:** Option C leaves the race in place
and repairs its loser. If Lakko later finds that multi-admin concurrency is common
rather than incidental — large moderated groups, ADR-0028 invite links at scale,
ADR-0010 channel co-publisher groups — the retry-collision rate rises and Option B
becomes the right answer. Option C does not foreclose it; the intent-level retry
Option C introduces is, in fact, the same "express the intent, let a committer
land it" shape B generalises.

---

## Why the choice moved to Option B

The recon's case for C was proportionality: *"not proportionate to this hazard
today."* **Proportionality is a frequency claim.** The ADR was careful enough to
refuse to invent a frequency, and then leaned on one anyway — that is the flaw in
its own reasoning, and it is worth naming plainly rather than quietly fixing.

The maintainer's correction supplies what the recon left out, and it is about
**scale**:

1. **The recon reasons about one device forking from one group.** In a large group
   with many admins, concurrent membership changes are not an exceptional event —
   they are the ordinary moderation load. Every round of concurrency produces
   **one winner and N-1 broken devices**, not one. The hazard's cost is linear in
   the number of admins acting, and the recon priced it as if it were constant.

2. **Option C's convergence degrades in exactly the same variable.** C converges
   in as many rounds as there were concurrent committers, and every loser detects
   its loss at the same moment (the winner's commit is one journal row, fetched by
   everyone on the same poll) and therefore retries at the same moment — so they
   **collide again**. C needs randomised backoff and jitter to converge at all
   beyond two committers. The recon's sprint list does not mention backoff, and its
   own honest-limitations list says only that the retry budget "bounds the
   livelock; it does not eliminate it." At two admins that is a footnote. At ten it
   is the design.

3. **"Retry" is the wrong verb for the commonest trigger.** The recon's own list of
   correlated triggers is a list of *multi-admin* situations, and the second entry
   is the sharpest: two admins approving the **same** pending join, which ADR-0027's
   members panel shows to every owner and admin simultaneously and which ADR-0028's
   invite-link flow is designed to produce. When that race resolves, **the loser has
   nothing to redo — the person is already a member.** Replaying the intent would
   add them twice. So C does not merely need a retry; it needs an **"is my intent
   already satisfied?" idempotency predicate, written per intent type** — add,
   remove, role change, approve — each with its own definition of satisfied, each
   evaluated after a whole-provider rollback and replay. That is a materially bigger
   and more delicate piece of work than "retry the intent", and the recon does not
   cost it.

**What flips the decision is that the hazard's frequency scales with (admins ×
churn) while Option C's convergence degrades in the same variable.** A repair whose
cost grows with the same term as the fault it repairs is the wrong shape. Option B
does not resolve the collision repeatedly and more expensively as the group grows —
it removes the collision.

And the structural reason B is the right target, restated from
[§ The load-bearing framing](#the-load-bearing-framing): **B moves the retry from
the expensive artifact to the cheap one.** A proposal that loses its race is not a
fork and needs no repair — its author re-proposes against the new epoch, with no
rollback, no retained snapshot, no replay, and no idempotency predicate, because
re-proposing an already-satisfied intent is caught by the ordinary
"is this member already in the group" check the UI does anyway.

---

## Decision

**Build Option B: proposal-based commits with a designated committer.** Members
emit MLS Proposals; exactly one elected owner-or-admin sweeps them into a single
Commit. Option A (a DS-side epoch gate) is **not built**, for the reasons the recon
gives and which the choice of B does not disturb. Option C is **not built as a
standalone option**, but **its committer rollback survives as a required part of
B** — though not in the form C described it: what was built is
`stage_pending_proposals` / `merge_staged_sweep` / `abandon_staged_sweep`, which
defers the eager merge rather than retaining and restoring a snapshot. See
[§ The staged sweep, as built](#the-staged-sweep-as-built). Option C′ (re-read the
journal head before committing) is **absorbed into B** as the committer's
stand-down rule, not shipped as a separate companion — ~~and it is client
orchestration that does not exist yet~~ **and it is now built on all three
clients**, immediately before staging, standing down on **any** unapplied row
rather than only on a commit (the reason is in
[§ What the client sprints found](#what-the-client-sprints-found), item 2).

Scope: **conversation groups** (`lakko_mls::Group`). Explicitly **not** DeviceGroups,
**not** call groups, and **not** channel `PublisherGroup`s — see
[§ What Option B does not fix](#what-option-b-does-not-fix), which states why for
each and what the residual is.

---

## The Option B design, settled

The recon named B's blockers and left them open. This section resolves each, with
a recommendation, grounded in the code as it stands.

### Decision 1 — who commits

**Recommendation: the ADR-0022 role table decides. `elect_committer` does not
transfer; its *shape* does.**

The committer is the **owner**; if the owner is not acting, the acting committer is
the **first admin in the role table's existing canonical order**. Both the
authorised set and the ordering are already there:
`RoleTable` (`crates/lakko-mls/src/group_roles.rs`) holds one owner plus an admin
set that `RoleTable::new` **sorts ascending and deduplicates**, and the whole table
lives in the authenticated `0xFF02` GroupContext extension — so every member holds
the identical bytes and derives the identical order with no election protocol, no
election message, and nothing new on the wire. **Rank 0 is the owner; rank k is the
k-th admin in the table's own order.**

**Why not `lakko_calls::link_invite::elect_committer`
(`crates/lakko-calls/src/link_invite.rs:230`) directly.** Two reasons, and only the
second is disqualifying on its own:

- **Its presence input has no chat equivalent *as it stands*.** Its input is
  `CommitterCandidate { leaf_index, is_owner, is_present }`, and `is_present` means
  *connected to the SFU room* — a live, shared, server-observed signal that exists
  only because a call has a server-mediated room, which a conversation group does
  not have. **This is a missing input, not a missing capability**, and the
  distinction matters: the chat path does carry presence signals today (ADR-0024
  typing and read receipts), and `lakko-presence` is a live exported surface —
  decision 2 sets out precisely what is there, what is genuinely absent, and what
  it would take to supply this input.
- **It keys on leaf index, and leaf indices renumber.** ADR-0022 keyed the role
  table on the Ed25519 signature public key *specifically* because OpenMLS reuses a
  vacated leaf for the next Add. A committer identity that can silently change
  ownership on every membership change is precisely the wrong stability property
  for the one role that must not be ambiguous.

**What transfers is the discipline**, and it is the valuable part: a **pure, total
function of state every member already holds**, owner-first, deterministic
tiebreak, recomputed on every change, requiring no negotiation and therefore unable
to split-brain on the *identity* question. Recommendation: a new pure function
beside the table it reads — `lakko_mls::group_roles::committer_rank(table, pubkey)
-> Option<u32>` — returning a rank over signature **public keys**, never leaf
indices, `None` for a plain member.

**With several admins:** all of them are authorised, exactly one is acting. Rank
orders them; decision 2 says how the rank is consumed. Note what this does *not*
do — **it does not widen the authorisation surface**. The committer set is exactly
the ADR-0022 set that may commit today. B **narrows** who commits (from "any
owner/admin, unilaterally" to "one elected owner/admin") while leaving who *may*
commit untouched. That is worth stating because the recon's cost item 1 described B
as relocating the authorisation surface; for the *commit* it does not relocate it
at all. What relocates is the check on the **proposals inside** the commit, which
is decision 3.

### Decision 2 — liveness

**Recommendation: the active admin devices elect the committer, it sweeps the
group's pending changes, the next candidate takes over when it is not acting, and
if none of them is running the request simply waits.** A membership request is
never abandoned, never expires and never fails for want of a committer.

> **This replaces an earlier revision of this decision, and the replaced design's
> failure modes are the reason this one is simpler.** That revision gave the wait a
> deadline: a per-rank activation delay built from two constants (`owner_grace`,
> then a per-rank `step`), a bounded retry budget for a request that kept missing
> sweeps, and a terminal *stalled — no admin available* state once every rank's
> delay had elapsed with nothing landing. Each was an unmeasured number standing
> between a user and a change they were already authorised to make, and the
> terminal state was the worst of them: from inside a client, *the budget ran out*
> and *the admins are asleep* are the same observation, so the design converted a
> wait whose end it could not see into a failure it asserted was final. It had no
> basis for that assertion. **Two of the three numbers go with it** — the retry cap
> and the stall threshold, and with the threshold goes the whole *stalled* state.
> Whether the third goes too is the open design question below.

**The rule, in the form that does not depend on any constant.** Each candidate
evaluates it independently, on every poll, over state it already holds:

> I am an authorised candidate. If this group holds an authorised, un-swept
> proposal, and no commit has landed since it, and **I am the lowest-ranked
> candidate currently known to be active**, then I sweep every authorised pending
> proposal into one commit.

Rank comes from decision 1 and is unchanged: `committer_rank(table, pubkey)`, a
pure total function of the ADR-0022 role table, which lives in the authenticated
`0xFF02` GroupContext extension. Every member derives the identical ranking from
identical bytes — there is no election message, no handover protocol, no committer
state and nothing new on the wire, which is exactly why the election cannot
split-brain on the question of *who* rank k is. Rank 0 is the owner, so with the
owner running the app it sweeps immediately and nothing else is ever consulted.

Everything that follows is about the one clause that is not free: **currently known
to be active.**

#### What presence the chat path actually has, stated correctly

An earlier draft of this section claimed a conversation group has no presence
signal. **That is wrong, and correcting it changes the design rather than the
wording.** The chat path carries two live presence signals today, both shipped on
all three clients:

- **ADR-0024 typing indicators** — a real per-`(group, sender_leaf)` ephemeral
  signal riding the group journal, with a ~6 s expiry, a ~4 s send throttle and a
  stale-ignore re-sync guard (`apps/web/src/chat/presence-ops.ts` — `foldTyping`,
  `activeTypers`, `pruneTyping` — driven by `apps/web/src/routes/Chat.tsx`, with
  the iOS and Android twins in each platform's `ChatClient` / `ChatRepository` /
  `ChatViewModel`).
- **ADR-0024 read receipts** — monotone per-leaf `read_up_to` watermarks in the
  same module (`foldReadWatermark`, `isReadByEveryOther`).

And `crates/lakko-presence` (blinded per-conversation presence tokens) is a real
crate with a **live exported FFI surface**: `crates/lakko-ffi-wasm/src/lib.rs:114`
re-exports `PresenceToken`, `PresenceTracker`, `derive_token` and `current_epoch`.
Two qualifications keep that honest — nothing under `apps/web/src` calls it, so the
**chat** path does not consume it; and it is exposed on **wasm only**, with no
UniFFI counterpart, so the natives would need that surface added.

**The genuine gap is narrower and sharper than "no presence".** Every signal above
is **positive-only and event-driven**: typing fires only while someone composes, a
read receipt only when they read. So *observing* a signal proves a member is
online, while *observing none* proves nothing at all — and an admin sitting idle
with the group open, which is precisely the candidate the sweep wants, emits
nothing. The missing thing is not presence infrastructure; it is a **continuous,
unconditional liveness beacon**.

#### The design question this leaves — **ANSWERED: variant 1, and it is built in the core**

> **This section was written as an undecided trade and is kept in that voice,
> because the trade is what the choice was made against.** The maintainer chose
> **variant 1, the candidate heartbeat** (`3f09ca1`), and it has since been built
> in `crates/lakko-contacts` and exposed on both FFI surfaces — see
> [§ The beacon, as built](#the-beacon-as-built), which records what the build
> settled that this section could only pose. **Variant 2 is kept as the documented
> fallback, not deleted.** Nothing below is retracted: the cost stands as recorded
> and was accepted with it, and the correctness caveat two sections down is
> **unchanged by the choice and must not be read as softened by it**.

**Variant 1 — a candidate heartbeat, and the maintainer's model becomes literal.**
Each owner/admin device publishes a small periodic liveness signal for the group
while the app is foregrounded. Then "active admin devices elect the committer" is
implementable as written: each candidate computes the active set from beacons seen
within a freshness window, and the lowest-ranked active candidate sweeps. **The
stagger, and its constant, disappear entirely** — nobody waits a turn, because the
set is known rather than inferred. ~~It is not built, but it is~~ **It is now built
in the core, on both FFI surfaces and on all three clients, and it was**
**well-precedented rather than
blocked**: typing indicators are the working proof that a throttled ephemeral
per-group journal signal with an expiry window functions, and `lakko-presence`
exists for exactly this shape — **though the build did not use it, and
[§ Why not `lakko-presence`](#why-not-lakko-presence) records why that sentence,
written here in good faith, turned out to be the wrong instinct.**

**The cost, recorded because it is why this is a decision and not an obvious yes.**
ADR-0024 flags its own mechanism as an anti-pattern in its Consequences: typing
signals are journal rows, permanent until the 30-day prune, *"mitigated by
throttling + tiny payload"*. A heartbeat is the same shape but **continuous rather
than event-driven**, so it multiplies exactly the cost ADR-0024 already accepted
grudgingly — every candidate, in every group, for as long as the app is open. And
it publishes something typing does not: a **continuous record of each candidate's
activity schedule**, visible to every member of the group and retained for the TTL.
Typing leaks that you were composing; a beacon leaks when you have the app open,
always. Naming that is this ADR's job; choosing it is the maintainer's.

**Variant 2 — no heartbeat, and the takeover is staggered.** Absent a beacon, a
device cannot *choose* the next committer; it can only *become* one. The honest
translation of "if the committer is offline, the next one is simply chosen" into
something implementable is then:

> the next candidate waits its turn and sweeps if nobody else has.

That turn is the surviving constant. It is **not** a timeout on the user's request —
the request has no timeout and nothing about it is judged when the interval
elapses — it is purely the stagger between candidates, and **without it every admin
sweeps at once, which is the concurrent-commit race this ADR exists to remove,
reproduced by its own fix.** What is observed is **inaction, not absence**: a
proposal has been sitting in the journal and no commit covering it has landed. That
is the only signal available without a beacon, and it is the one the user cares
about either way.

**Recommendation: build variant 1, and keep variant 2 as the fallback rather than
the design** — a group in which no candidate has beaconed recently (older clients,
a fresh install, a beacon the maintainer decides not to ship) must still make
progress, and staggered takeover is what makes it do so. Note the two are the same
rule with different inputs: variant 1 knows the active set, variant 2 infers a
prefix of it by waiting.

**In either variant, measure — do not guess.** If the stagger ships, its value must
exceed a DS publish round-trip by a comfortable margin *plus* the expected
client-to-DS clock offset, and must exceed the group's normal poll cadence so a
candidate merely between polls is never jumped by the one behind it. That cadence is
not uniform today — Android's `pollGroup` awaits a deliberate 30 s long poll while
the channel co-publisher loops poll on a 5 s interval — which is itself an argument
for measuring. **This is also where the replaced `owner_grace` went:** its stated
constraint was exactly this one, and it applies identically to every gap in the
order, so the two were never independent numbers. A heartbeat's freshness window is
the same measurement problem in a better place: it bounds a *positive* observation
rather than licensing an inference from silence.

**The age is measured against the DS's clock, never the proposer's.** Every fetched
journal row carries a server-assigned `created_at TIMESTAMP(3)`
(`services/ds/migrations/0001_init.sql`, returned on `FetchedMessage` in
`services/ds/src/journal.rs`), and every member reads the identical value on the
identical row, which removes the proposer's clock from the measurement entirely.
What remains is each evaluator's own offset from the DS clock — a real residual, and
in variant 2 the reason the stagger must dominate it.

#### The beacon, as built

Landed in four commits: `94825ec` (the core envelope, the fold, the throttle, the
dispatch variant, the fuzz target), `94da492` (both FFI surfaces, `Group::committerRank`,
and the slim `lakko-ffi-extension` classify surface), `5d6af94` (the three background
notification filters), `f53d11a` (the three foreground timeline filters). The module
documentation in `crates/lakko-contacts/src/committer_beacon.rs` is the authority;
this section records the decisions it settled, because they are decisions this ADR
posed and did not answer.

**The emitter and the observation store came later and are on all three clients**
(`02f0037` web, `1f230ab` iOS + Android), which is what turned this from a mechanism
into an election. The two rules the clients had to add on top of the core's are
pinned as **C-64** and are both about where an input comes from: a beacon's rank is
re-derived through `Group::committerRank` **at verdict time and never at receipt**
(a rank is a *position* in the role table's canonical order, so any promotion,
demotion or removal renumbers every candidate sorting after it — a stored rank is not
stale, it is a position in a different ordering), and a beacon is emitted **only from
a foregrounded client that can actually sweep**, which the core cannot check and
which rests on C-59.

**The wire shape — a fixed 27 bytes, and the payload carries no fields at all.**

```text
  plaintext_payload = "lakko/committer-beacon/v1\0" || 0x01
                     └──────── 26 bytes ────────┘   └ version ┘
```

It rides an ordinary **MLS application message** on the group journal, so the DS
sees ciphertext (invariant #2) and the whole payload is already AEAD-sealed under
the group's epoch secret. The codec is bounded and fail-closed — domain match, then
**exact** length, then the version gate — dispatched by `incoming_envelope` as its
own **unit** variant `IncomingEnvelope::CommitterBeacon`, byte-distinct from every
sibling envelope domain, and fuzzed (`committer_beacon_decode`).

**Being field-less is the decision, and the timestamp is the sharp half of it.**
Three fields a beacon might plausibly carry were each excluded for a reason:

- **Identity.** Already MLS-authenticated via the sender leaf. Naming itself in the
  payload adds a second, *unauthenticated* claim of identity beside the
  authenticated one — a spoofing surface and nothing else.
- **A timestamp.** This is the one that matters. Age is measured against the DS
  row's server-assigned `created_at`, which every member reads identically on the
  identical row. A self-reported time is not merely redundant but **wrong**: it
  would let a candidate hold the committer duty by **asserting its own freshness**,
  a liveness claim **no peer can check**. Against the negative consequence this ADR
  already records — *an elected committer can selectively censor proposals* — that
  turns a duty nobody can verify into a duty nobody can take away.
- **A claimed rank.** The receiver re-derives rank from the authenticated role
  table, so a claimed rank would have to be ignored, and carrying a field every
  correct implementation must ignore is an invitation for one implementation to
  trust it.

What remains is the signal itself, whose entire content is *"the device at this
sender leaf was foregrounded and able to sweep when the DS stamped this row"*, plus
the version byte as the forward-compatibility knob — the same knob ADR-0044's v2
bump proved load-bearing on the sibling push-routing envelope.

**The fold and the throttle are pure, total and clock-free, and live in the core
once.** `acting_committer_verdict(own_rank, observed, freshness_window_ms)` →
`ActingCommitter::{Act, Defer { to_rank }, NotACandidate}`, and
`should_emit_beacon(own_rank, last_emitted_at_ms, now_ms)`. Both are consumed over
**both** FFI surfaces rather than re-derived per platform, for the reason ADR-0044's
F-A replay made concrete: three clients each hand-rolling one rule is the drift
class. Six rules govern the fold, and the sixth is the one most likely to be got
wrong — **hearing nothing means `Act`**: silence means nobody better placed is
running, which makes this device the answer, not a failure to elect. `Defer` and
`NotACandidate` are deliberately distinct variants rather than one `false`, because
one has something to re-evaluate on the next poll and the other never will.

A `BeaconObservation` carries only `rank: Option<u32>` and `age_ms`, and **both are
the observer's own re-derivation** — rank from the MLS sender leaf resolved through
`members()` and ranked by `Group::committerRank`, age from the DS row. Nothing in
the payload contributes to either, which is what the field-less shape buys.

**`Group::committerRank(signatureKey)` is a `Group` method, not a free function
taking a caller-supplied table, and the reason is a correctness one.** Both FFI
surfaces carry it as a doc heading: *rank the key when you take the verdict, not
when the beacon arrived.* A rank is a **position** in the role table's canonical
order, not a stable identity — promoting, demoting or removing any admin renumbers
every admin sorting after it — so a projected observation set that stored ranks at
receipt would be mixing positions from **different orderings**, and comparing them
against this device's own rank (which is necessarily current) is meaningless rather
than merely stale. A free function would have let a caller supply a table it had
cached; the method reads the group's live one. Callers persist the sender's 32-byte
**signature key**, never a leaf index, because OpenMLS reuses a vacated leaf for the
next member added. This orders the authorised committers; it does **not** widen them
— the set that may commit stays exactly ADR-0022's owner-plus-admins.

**The two constants are CHOSEN, not measured, and the module says so in those
words.** `COMMITTER_BEACON_INTERVAL_MS = 60_000` and
`COMMITTER_BEACON_FRESHNESS_WINDOW_MS = 2 * interval + worst_case_poll = 150_000`,
with `COMMITTER_BEACON_WORST_CASE_POLL_MS = 30_000` named as its own term rather
than folded silently into one number, because it is a **constraint on the caller**:
a client that polls slower than that invalidates the window's derivation. This ADR
asked for measurement (*"In either variant, measure — do not guess"*) and did not
get it; what it got instead is a **budget argument**, stated so a later measurement
can overturn it:

- **The latency budget is not interactive.** Nobody watches a beacon. What waits on
  it is a membership change decision 2 deliberately gave **no deadline at all**. So
  the interval is sized for litter and privacy, not responsiveness — the opposite of
  ADR-0024's ~4 s typing throttle, whose whole job is to look instant to a human.
- **It is a litter budget.** At 60 s that is 60 journal rows per hour per candidate
  per group, persisting to the 30-day prune. Halving the interval doubles that for
  no benefit anyone is waiting on.
- **It is a privacy budget.** Each row publishes that this candidate had the app
  foregrounded at that moment. A shorter interval buys a **finer-grained** activity
  schedule — a real cost with no matching gain.

The window's three terms each earn their place (one interval, because a perfectly
healthy candidate's newest beacon is already that old the moment anyone looks; a
second, to tolerate exactly one lost or delayed beacon rather than evicting a live
candidate on a single dropped round; plus one worst-case poll, because a beacon is
only *observed* at the observer's next poll), and the arithmetic is pinned by
`freshness_window_is_the_documented_construction` so shrinking the window below the
tolerate-one-loss point fails loudly instead of quietly making takeovers flap.

**Foregroundedness is a caller obligation the core cannot check**, and both FFI
surfaces say so at `composeCommitterBeacon`: a beacon must mean *able to sweep*, and
the ADR-0041 background decrypt is read-only-throwaway and structurally cannot
commit (**C-59**), so a beacon emitted from the background is a lie that costs the
group a takeover.

#### Why not `lakko-presence`

The crate exists, ships blinded per-conversation presence tokens, and is already
exported on the wasm FFI — which is precisely why reaching for it is the obvious
move, and why the build recorded four reasons for not doing so. The two sharpest:

1. **A presence token is not authenticating.** `derive_token` is keyed on a seed
   **every member holds**, so any member can mint any other member's token; the
   crate says so itself. Identification is by *recomputing* the expected token for
   each candidate pseudonym and matching — an O(members) scan yielding a
   **pseudonym**, where the fold needs a **signature key** to look up
   `committer_rank`. MLS already hands the receiver an authenticated sender leaf for
   free. Swapping an authenticated attribution for an unauthenticated one, to obtain
   something the transport already delivered, is a strict loss.
2. **Its clock model contradicts this ADR.** `derive_token` binds a wall-clock epoch
   from the **sender's** clock into the token, and a receiver must recompute with
   the same epoch to match at all. This ADR is explicit that age comes from the DS
   row's `created_at`, never the sender's clock. Adopting tokens would put the
   sender's clock back into the decision as a **matching precondition**: a candidate
   with a skewed clock emits tokens nobody can match and reads as permanently
   absent — silently, and exactly when it is the device that should be sweeping.

The other two: the blinding hides *who is present* from an observer lacking the
group secret, which is confidentiality an enclosing MLS application message already
provides, so it would be applied *inside* the layer that supplies it; and the crate
is not actually wired — its own docs place sourcing `presence_seed` from an MLS
exporter outside itself, that exporter label does not exist, nothing under
`apps/web/src` calls it, and there is no UniFFI counterpart. Adopting it would have
meant building a new MLS exporter, a pseudonym enumeration and a second FFI surface
in order to obtain a weaker attribution than the one already in hand.

**What would make it the right primitive, recorded so the crate is judged rather
than dismissed:** a beacon that had to travel *outside* MLS — to non-members, or
over a channel the server can read — is exactly what the blinding is for, because
then there would be no enclosing layer providing confidentiality and no
authenticated sender attribution to lean on. Inside MLS there is both.

#### The filter obligation the beacon created, and why a beacon is an event rather than a drop

Neither this ADR nor the beacon design anticipated this, and it is the larger half
of the client-side work that landed. A beacon is an **ordinary MLS application
message**, and `lakko_attachments::parse_body` carries a **deliberate** legacy
fallback: bytes that are not the ADR-0023 JSON envelope degrade to
`MessageBody::Chat` via `String::from_utf8_lossy`, which is what keeps pre-1.19
plain-UTF-8 messages readable. Fed a beacon, that fallback yields "chat text"
reading `lakko/committer-beacon/v1` plus two control bytes — on the timeline, and in
a push notification. The fix belongs in the clients and not in `parse_body`: the
fallback is load-bearing for wire compatibility, and `lakko-attachments` does not
depend on `lakko-contacts`, so teaching it these domains would invert the crate
layering.

This is **more** reachable than the ADR-0042 push-routing case that first hit it,
not less: a routing secret is broadcast on a membership change, while a beacon is
emitted by every owner and admin every interval, so in an active group the newest
journal row — the one a notification previews — is frequently a beacon.

So all three clients gained an arm ahead of the chat parser on **both** paths:

- **Background** (`5d6af94`) — the web service worker's `sw-push-decrypt.js`, the
  Android `PushServiceEnvelopeFilter.isServiceEnvelope`, and the iOS NSE's
  `nseIsServiceEnvelope`. Each is a cheap peek (exact length + the leading `0x6c`)
  gating an authoritative FFI decode. The iOS NSE reaches the codec through the slim
  `lakko-ffi-extension` surface, which exports `decode_committer_beacon` **and the
  envelope length** so `27` is not hand-copied into Swift where nothing would pin
  it — a stale literal there fails in the **dangerous** direction, silently ceasing
  to suppress beacons. `golden_pins_the_swift_prefilter_shape` pins the exported
  value against literals so a wire revision fails Rust CI rather than a
  notification.
- **Foreground** (`f53d11a`) — the web `chat/client.ts`, iOS `ChatClient.swift` and
  Android `ChatClient.kt` classifiers, same two-step shape.

**Unlike the other envelope filters, the peek here is a complete test rather than a
fast path**, because the beacon is field-less: every beacon on the wire is
byte-identical, so exact length plus the domain prefix already pin all but the
version byte. The codec behind it confirms — and is what keeps the arm **fail-closed
on an unknown schema version**: a future v2 beacon this build does not understand is
not silently swallowed, it falls through to the chat parser exactly as any other
unrecognised plaintext does.

**A recognised beacon surfaces as its own event variant rather than being dropped in
the parser**, on all three clients, and that is the load-bearing shape rather than a
stylistic choice: the two things the fold needs — the **MLS sender leaf** and the
**DS row's `created_at`** — are precisely what the field-less payload deliberately
does not carry, and they exist only at that point in the receive chain. A filter
that dropped the row would be discarding the entire signal in the act of
recognising it. Each client therefore emits a `committerBeacon` event carrying
`senderLeaf` and `createdAt`, which the receive loop currently consumes and does not
render; that event is the seam the fold will attach to.

The two native builds also needed an exhaustive-switch repair (`5d6af94`): the
call-path `MultiJoinerCall` dispatchers gained a `committerBeacon` arm that drops
the payload, because a beacon rides the **group journal** and never the SFU
signal-relay, and it says nothing about a *call* committer — ADR-0032's election is
leaf/presence-based and independent.

#### Does the beacon earn an indexed invariant? **Yes — and it is minted as C-64**

> **This section previously read "Not yet — and the trigger is named". The trigger
> has fired.** The reasoning below is kept in full rather than rewritten away,
> because the *argument* for deferring was correct at the time and the shape of what
> was minted is exactly what that argument predicted.

Argued rather than defaulted, because the case for one is real. **For:** the
no-self-asserted-freshness property is genuinely security-shaped and genuinely
breakable — a later hand adding a `sent_at` field "for diagnostics" would hand a
malicious admin an unfalsifiable claim to hold the committer duty, against a
censorship authority this ADR already lists as a negative consequence. It has the
same shape as C-60 sub-invariant 2 (a domain-separated, bounded, fail-closed
in-group envelope on an MLS application message).

**The deferral, and why it lapsed.** The argument against minting was that the
invariant index's own quick-stats row describes its contents as **live properties**
and this mechanism was **inert** — no client emitted a beacon and none drove the
fold — so there was no running enforcement point to pin, and indexing it would have
made the index aspirational, exactly the drift the index exists to prevent. That
condition was named as a **trigger**, not as a permanent judgement: *mint it at the
first client that emits a beacon and folds a verdict*. **S4b (`02f0037`) is that
client and S5/S6 (`1f230ab`) are the other two**, so the deferral is spent.

**And the mint is broader than the beacon, exactly as this section predicted it
would have to be.** The prediction was that "the property worth indexing is broader
than the beacon alone — the integrity of the committer election as a whole — so a
narrow C-number minted now would need re-scoping then". Building the clients bore
that out twice over. The beacon on its own still authorises nothing and gates
nothing: the worst a forged or replayed one can do is make a device **defer**, a
liveness effect of the class this pack tracks as a gap rather than an invariant (cf.
ADR-0038's fail-toward-UpToDate as G-32). What the clients added *around* it is not a
liveness effect — a sweep that merges before the journal has ordered its commit is
the device-ahead brick this ADR exists to close, and a Welcome published before that
verdict is strictly worse than the two-party fork. So **C-64 pins the election and
the sweep together**: the fold's inputs, the emitter's foreground obligation, the
stand-down, the stage-verdict-adjudicate ordering, and the Welcome-after-verdict
rule. Its two originally-named properties are sub-invariants 1 and 3 of it.

For the same reason as the original deferral, the **service-envelope suppression
filter** is *still* not indexed — and note that its ADR-0042 predecessor never was,
so indexing the beacon's half alone would be incoherent. It is live on six code paths
and its failure mode is user-visible rather than confidentiality-affecting; if it is
ever indexed it should be indexed whole. That is a judgement, not a measurement, and
it is the same one this section made before the clients landed.

#### What stops two devices both sweeping — and why a heartbeat does not close it

1. **Deterministic rank, from authenticated shared state.** There is nothing to
   agree on about *who* rank k is (decision 1). The one exception is two devices
   holding the *same* signature key — see
   [§ What Option B does not fix](#what-option-b-does-not-fix), item 6.
2. **A bounded active set** — the beacon freshness window in variant 1, the stagger
   in variant 2. **Neither is exact, and this is the load-bearing caveat:** a
   heartbeat makes the election's *input* approximate, not certain. Each candidate
   computes over its own view of who has beaconed recently, those views differ with
   poll timing, and a beacon is stale by up to one interval — so two candidates can
   still both conclude they are the lowest-ranked active one. **A heartbeat removes
   the wait; it does not remove the race.** Anyone reading variant 1 as
   correct-by-construction has mis-read it.
3. **Stand-down on observation.** Immediately before creating the commit, re-read
   the journal head; if any commit has landed at the current epoch, abandon and
   re-evaluate. This is Option C′, and under B it is not a companion mitigation but
   a **required step of the algorithm** in both variants — it is what makes the
   election effective rather than merely probable, and it costs one fetch on a path
   that only runs when the owner is not the one acting.

   > **Built on all three clients, and built one notch more conservative than this
   > text.** The clients stand down on **any** unapplied journal row, not only on a
   > commit, because no FFI surface can tell a commit from an application message
   > pre-decrypt and the two mistakes cost very different amounts: standing down on
   > a row that was not a commit costs one retry, while staging on a row that *was*
   > costs a commit every peer drops. See
   > [§ What the client sprints found](#what-the-client-sprints-found) item 2.
4. **The residual is repaired, not prevented.** If two candidates do collide, one
   forks — the hazard this ADR exists to close, in a much narrower window. So the
   acting committer must be able to survive losing the journal order across the
   publish→verdict window. Required in both variants.

   > **This is the one place the built core diverges from what this ADR
   > prescribed, and the divergence is the better answer.** The text this
   > paragraph replaces said the committer "retains its pre-commit epoch-N
   > snapshot and rolls back if it lost — Option C's steps 1, 3 and 5". **No such
   > snapshot exists and none was needed.** The rollback point is not a captured
   > artifact; it is the **absence of the eager merge**. See
   > [§ The staged sweep, as built](#the-staged-sweep-as-built).

#### The staged sweep, as built

`Group::stage_pending_proposals` builds the sweep commit and **stops before
merging it**, so the group sits at epoch **N** holding an un-merged staged
commit. The caller publishes, reads the journal verdict, and then calls one of:

- `merge_staged_sweep` — **won**, land at N+1, the state the peers reached;
- `abandon_staged_sweep` — **lost**, discard the staged commit and stay at N,
  free to apply the winner and carry on.

`commit_pending_proposals` is exactly the pair run back-to-back. It is kept
unchanged for callers that have not moved onto the staged pair, and its behaviour
is byte-for-byte what it was — but a committer that can lose a race must not use
it, because merging before the journal has ordered the commit is precisely what
strands the loser.

**Nothing is captured and nothing is persisted separately.** Abandoning is
OpenMLS's `clear_pending_commit`, whose entire effect is to set this group's
`group_state` back to `Operational`. That matters beyond tidiness: a
whole-provider `Identity::restore_storage` — the mechanism Option C's step 5
called for, and the one ADR-0040's abort path uses — restores **every** group on
the account, so using it to undo one group's commit would roll sibling groups
back past commits they have already published. Per-group staging has no such
blast radius, and `abandoning_a_sweep_leaves_a_sibling_group_untouched` pins it.

Nor is any work thrown away: OpenMLS empties the proposal store on **merge**, and
an abandoned commit was never merged, so the proposals the sweep selected are
still pending for the winner's next sweep
(`an_abandoned_sweep_leaves_its_proposals_for_the_next_committer`).

**This is ADR-0040's own deferred "Option B" — defer the eager merge in the core
— scoped to the sweep path only.** ADR-0040 weighed it, called it *"cleaner in
principle"*, and rejected it for v1 because it *"touches the sacrosanct MLS core
… and both FFI surfaces"*;
[§ Alternatives rejected](#alternatives-rejected) below repeats that framing and
is now out of date in exactly that respect. What landed is the narrow version:
the four **direct**-commit paths keep merging eagerly and behave byte-for-byte as
before (`a_rollback_cannot_reach_an_eagerly_merged_commit`), and only the new
sweep path defers.

**One expectation of ADR-0040's does not carry over, and it matters.** ADR-0040
supposed that folding in defer-merge would *"remove the outbox's re-publish
machinery"*. It does not: the outbox is still required, because the two mechanisms
answer different questions about the same commit — the outbox answers *did my
commit reach the wire*, the staged pair answers *did it win the wire order*. The
first must be *yes* before the second is meaningful. So the client sequence is
**durably record → publish (or re-publish) → read the verdict → merge or
abandon**, and ADR-0040's outbox wraps both commit shapes unchanged.

**Two properties of staging are new client obligations, and neither was in this
ADR before the sprint landed.** Both are discharged on all three clients, and
[§ Consequences](#consequences) records how; the point of stating them here is that
they are obligations a later refactor can drop, not facts the core enforces.

**Two more were added by the security review, and they are listed here rather than
only in that section because this is where a client author looks.** Both are stated
in full as items 14 and 15 of
[§ What the security review found](#what-the-security-review-found):

- **An artifact still owed after the merge needs its own durable name.** The
  Welcome-after-the-verdict rule below is an *ordering* rule and stays exactly as
  written; what it was missing is that the committer must **record that the verdict
  settled** before attempting the fan-out. Otherwise a failed publish is
  unrecoverable, because the merge has already cleared every signal the recovery
  paths branch on.
- **The receiver has obligations too, and this ADR had never stated any.** It
  enumerates what a *committer* owes and is silent on the other side — yet the arc
  made a receiver's durable proposal store a precondition for applying a later
  by-reference commit. The rule is that **the journal cursor may lag the durable
  MLS state but must never lead it**: persist fail-closed first, advance the cursor
  second, hold it when the write does not land.

- **A staged sweep freezes that group's own membership operations.** OpenMLS's
  `is_operational()` returns `MlsGroupStateError::PendingCommit` while a commit
  is staged, and it guards `add_members`, `remove_members`, `self_update`,
  `update_group_context_extensions` and all three `propose_*` entry points. So
  between staging and the verdict this device can neither propose nor commit
  anything else in that group. `stage_pending_proposals` refuses a **second**
  stage for the same reason, fail-closed — building one would silently replace a
  commit that may already be on the wire.
- **The staged commit is durable.** `stage_commit` writes
  `MlsGroupState::PendingCommit` through `write_group_state`, and `MlsGroup::load`
  reads it back. That is what makes an ADR-0040 restart sweep able to re-publish
  and re-adjudicate after a crash — and it is also what turns the freeze above
  from an advisory into a **binding** obligation: a client that stages and never
  reaches a verdict leaves that group unable to change membership **across
  restarts**, not merely until the process dies.

  **Incoming messages are unaffected** — nothing on the receive path consults
  `is_operational`, and applying the winner's commit works whether or not the
  loser has abandoned yet, because OpenMLS discards a pending commit as part of
  merging a foreign one. Abandon-then-apply is the honest order and the one this
  crate drives; the reverse is pinned as a safety net
  (`applying_the_winner_before_abandoning_still_converges`) so a client that gets
  the order wrong still converges rather than wedging.

**That rollback is materially smaller than Option C's, and it needs no budget** —
which is why deleting the retry cap costs nothing:

- **No intent replay, therefore no idempotency predicate.** The committer's intent
  is "sweep whatever is pending". After rolling back and replaying the winner's
  commit it simply re-evaluates the rule — and the winner's commit almost always
  swept the same proposals, so there is nothing left to do. The per-intent "is my
  intent already satisfied?" problem that
  [§ Why the choice moved to Option B](#why-the-choice-moved-to-option-b) item 3
  raises against C **does not arise**, because the proposals themselves are the
  durable record of intent and the winner consumed them. A retry budget would be
  bounding a loop that does not exist.
- **No add-path KeyPackage question.** See [§ Open questions](#open-questions) — the
  discovery KeyPackage directory is non-consuming today, and under B the fetch
  happens once at *propose* time regardless.
- **No approve-path stale-proposal question.** The external-join proposal is not
  held in a Lakko-side in-memory queue under B; it is a stored MLS proposal like any
  other (decision 4). **That is true of B as designed and it is not what S7 built** —
  S7 (`576ea14`) landed the convergence but **kept** `pending_external_joins`
  deliberately, because the quarantine is what stops a `/g/` link-holder filling the
  member proposal store (see
  [§ Why decision 4's design does not survive](#why-decision-4s-design-does-not-survive)).
  So the in-memory `Vec` is still there and still outside the staged rollback. Two
  corrections to how this bullet described it: the retained entry is **not
  epoch-bound** — the retained `KeyPackage` outlives epoch advances, and only the
  joiner's *original* external proposal had to arrive at a matching epoch — and it is
  no longer dropped only by a restart, since `pending_external_joins()` filters
  members, so an approved joiner retires itself from every admin's list once the sweep
  lands.

#### Every candidate offline: the request waits

**This is the designed behaviour, not a failure state.** Nothing is running, so
nothing sweeps, and the request stays in every member's proposal store until a
candidate next opens the app. Nothing expires it, nothing counts against it, and no
client asserts anything about why. The user-visible state is *waiting for an
admin* — **one** state, not two, because no client can distinguish a two-minute wait
from a two-day one except by a threshold it would have to invent.

**A backgrounded candidate cannot sweep, and under this model that is benign.** The
ADR-0041 background decrypt is read-only-throwaway by construction — no persistent
provider handle, zero write transactions, and no `snapshot_storage` on the slim
`lakko-ffi-extension` surface at all (C-59) — so it **cannot commit**. A sweep
happens only while a candidate has the app in the foreground and polling, which
makes waiting reachable more often than "their phone is on" would suggest. (It also
bounds variant 1: a heartbeat can only be emitted from the foreground, so the beacon
correctly reports *able to sweep*, not merely *device awake* — which is the property
the election actually needs.) This is worth recording because under the replaced
design it was a real problem: a wait that outran the activation delays became a
terminal *stalled* state and one that outran the retry budget became a terminal
failure, both assertions the client had no basis for. With no deadline and no budget
the only consequence is a longer wait — which is what the maintainer's model says
should happen.

**Ownership transfer is built** (`e780952`). The replaced design listed ADR-0022's
deferred ownership transfer as a follow-up that B promoted from a nicety to
load-bearing; an earlier revision of this paragraph then said a sprint "is running
now" and that "this ADR does not yet know its API". It landed, and the API is:

- `RoleTable::with_owner_transferred(new_owner, outgoing)` — pure, total and
  **membership-blind**, because a role table names keys and cannot tell whether a
  key is in the group;
- `Group::transfer_ownership(identity, new_owner, outgoing)` — the direct-commit
  form, and `Group::propose_ownership_transfer(...)` — the proposal form;
- `OutgoingOwner::{Admin, Member}` — what becomes of the outgoing owner, stated
  rather than defaulted silently, because either silent default is wrong in one
  direction. `Admin` is the default.

**Transfer is deliberately not a new commit type.** It is the ordinary owner-only
role-table rewrite with the owner slot moved, so it rides the audited
`set_group_roles` / `propose_group_roles` paths and is judged by the existing
gate against the **pre-merge** table — the one in which the transferor is still
the owner. Nothing about who may mint one changes.

**What it does and does not resolve.** It removes the owner dead-end: the core
still refuses an owner `Leave` / self-`Remove` fail-closed
(`crates/lakko-mls/src/group_proposals.rs` — *"the group owner cannot leave"*),
and that rule is **unchanged** by transfer, because the way out is to transfer
first and leave as an ordinary member afterwards. The two cannot even be fused
into one commit: both proposals are judged against the pre-merge table in which
the transferor is still owner, so the `Leave` half is refused — which is also the
honest ordering, since the transfer must be durable before its author departs.
`transfer_then_the_former_owner_leaves_and_the_group_works` walks the whole path.

What it does **not** resolve is the dependent consequence in
[§ What Option B does not fix](#what-option-b-does-not-fix) item 2: **membership
can still stall.** Transfer needs the owner's key to mint, so it does nothing for
an owner who has *already* gone; it only lets one who is still present hand the
duty on deliberately. Recovering a lost owner is ADR-0022's separate,
still-open admin-quorum question.

~~**And it does not resolve it in the product either, because no client offers it.**~~
**It does now — the flow shipped on all three clients in `0274fa7`** (UX spec §6.1),
so "ownership transfer is built" is finally true of the app as well as the crate and
the owner dead-end is closed to a user. The design pass **settled** a question rather
than answering it, and that is the part worth keeping: the **direct arm is
structurally unreachable**, because `isOwner` and `usesProposalPath` are the same
read — the role table's presence — so every client drives `proposeOwnershipTransfer`
and none can reach `transferOwnership`. iOS proved it by execution rather than by
argument: driving the direct arm advanced the epoch **and stripped the transferor
locally**, so the next role change was refused as *proposer is not the owner*.

What survives of this paragraph is the part that was never about the affordance:
transfer needs the owner's **key** to mint, so shipping it does nothing for an owner
who has already gone. The stall in item 2 stands.

**The security content of that sprint was `owner_survives`, not the transfer
wrapper — and it closes a hole that predates it.** See
[§ Never leave the group without an owner in it](#never-leave-the-group-without-an-owner-in-it).

#### Never leave the group without an owner in it

`group_proposals::owner_survives` is the one rule in the crate that judges a
**commit** rather than a proposal, and it is checked against the roster the commit
*produces* — pre-merge members, minus everyone it removes, plus everyone it adds.
It refuses a commit that would install an owner who is not a member of the
resulting group, and a commit that rewrites the GroupContext without leaving a
readable `0xFF02` table behind.

**It is not a guard on a new feature.** `set_group_roles` has been owner-only and
free-form since ADR-0022: a role table names 32-byte keys, so an owner could
always hand-build a table naming a key that no leaf holds, and nothing refused it.
The result is permanent — a role change is owner-only so nobody could ever rewrite
the table, and the owner's leaf is unremovable so no removal could repair it (and
there is no such leaf anyway). `owner_survives` refuses that commit **on every
peer**, so it is a strict improvement independent of whether anyone ever calls
`transfer_ownership`. Pinned by
`a_peer_refuses_a_commit_that_hands_the_group_to_a_non_member` and
`a_peer_refuses_a_commit_that_erases_the_role_table`.

**The second reachable path is one this ADR created.** Because B composes N
intents into one commit, "hand the group to Charlie" and "remove Charlie" can ride
together, each individually authorised — the owner may rewrite the table, an admin
may remove a plain member — so no per-proposal rule can see it. Only the
post-commit roster can, which is why this rule cannot be folded into `authorise`.
The committer's own sweep excludes the role change rather than publishing a commit
every peer would refuse (`a_sweep_drops_a_transfer_to_someone_the_same_commit_removes`).

Indexed as **C-63**.

### Decision 3 — the proposal-authorisation gate (the invariant #3 piece)

**This is a change to the audited MLS state machine.** It requires this ADR,
property tests, and — per the repo's own rule for `lakko-mls` after Phase 2 — an
external crypto audit. It is the single riskiest part of B and the part where a
subtle miss silently re-opens ADR-0022's threat model.

**The hazard, precisely.** Today `process_incoming_verifying_roles`
(`crates/lakko-mls/src/group.rs`) does this with a proposal:

```rust
ProcessedMessageContent::ProposalMessage(_) => Ok(Incoming::Proposal),
```

The proposal is discarded, unexamined and un-role-checked. That is safe **only**
because Lakko stores no member proposals — verified: no call to
`store_pending_proposal`, `pending_proposals`, `commit_to_pending_proposals`,
`propose_add_member` or `propose_remove_member` exists anywhere in `crates/` or
`services/`, and all three clients classify a bare `Proposal` into a content-less
system row and act on nothing. A proposal that is never stored can never be swept.
**Option B stores them, and that safety evaporates the moment it does.**

**Recommendation: three layers, and be explicit about which one is the security
boundary.**

1. **Receipt gate (hygiene, not the boundary).** In
   `process_incoming_verifying_roles`, replace the discard with an inspection.
   `ProcessedMessageContent::ProposalMessage(Box<QueuedProposal>)` carries
   `QueuedProposal::sender()` and `::proposal()`
   (`openmls-0.8.1/src/group/mls_group/proposal_store.rs:169,187`), so the sender
   leaf resolves through `self.members()` to a signature key and through the
   pre-merge `RoleTable` to a role, exactly as `authorise_role_commit` already
   resolves the committer. Store it only if authorised; **drop it otherwise**. Two
   supporting facts make this the right layer to drop at: ~~`ProposalStore::remove`
   is `pub(crate)` in openmls, so there is **no public single-proposal eviction** —
   once stored, a proposal can only be cleared with `clear_pending_proposals`,
   which clears *everything*~~ — and openmls empties the store on every merged commit
   (`staged_commit.rs:549-552`), so no bespoke GC is needed for the ones that are
   legitimately stored.

   > **The struck half is factually wrong and is left visible rather than
   > quietly deleted.** `ProposalStore::remove` is indeed `pub(crate)`, but
   > `MlsGroup::remove_pending_proposal(storage, proposal_ref)` is **public**
   > (`openmls-0.8.1/src/group/mls_group/proposal.rs:502`), so a single-proposal
   > eviction does exist. The **conclusion is unaffected** — dropping at receipt
   > is still right, and no bespoke GC is still needed — and so is the UX spec's
   > "there is no `Cancel`", because that rests on there being no *wire* withdraw:
   > evicting locally would leave the request live in every peer's store. The
   > correction matters only so that a future reader does not treat "openmls
   > cannot evict one proposal" as an established fact.
2. **Sweep filter (the committer's own hygiene).** The committer must not use
   `commit_to_pending_proposals`. It must use `CommitBuilder::build(rand, crypto,
   signer, f)` with a real `f` that re-runs the authorisation against the
   **pre-merge** role table, so a proposal stored while its sender was an admin and
   swept after that sender was demoted is excluded. This is ADR-0022's Finding 2
   rule — judge against the table you inherit — applied to proposals.
3. **Peer-verified commit gate (this is the boundary).** Extend
   `authorise_role_commit` so that it authorises **each proposal's own sender**,
   not only the committer. This is the layer that matters, because **the committer
   is not trusted** — a malicious committer simply would not run layers 1 and 2.
   The inputs already exist: `StagedCommit::queued_proposals()`
   (`staged_commit.rs:650`) yields `QueuedProposal`s, and the per-kind wrappers
   `QueuedAddProposal::sender()` / `QueuedRemoveProposal::sender()`
   (`proposal_store.rs:743,762`) give the originating sender for by-reference
   proposals. `QueuedProposal::proposal_or_ref_type()` (`:183`) distinguishes a
   by-reference proposal (judge the **proposer**) from a by-value one (judge the
   **committer**, which is today's behaviour and must stay). Every existing rule —
   add needs owner/admin, remove needs owner/admin, the owner is unremovable, a
   role-table change is owner-only — moves from "the committer's role" to "the
   role of whoever authored this proposal", with the by-value case unchanged.

**This layering is ADR-0022's own, reused rather than invented:** the local check is
a courtesy that keeps the store clean and gives the user a fast local error; the
**receive-side refuse-to-merge is the security boundary**, and it is fail-closed
(invariant #4) exactly as it is today.

> **Correction from the build: for a by-reference proposal, layer 1 *is* a
> boundary.** This ADR calls layer 1 "hygiene" and layer 3 "the boundary". A peer
> that dropped a proposal at layer 1 has nothing for OpenMLS's reference
> resolution to find, so the commit carrying it is refused with
> `InvalidCommit(MissingProposal)` **before layer 3 is reached** — enforced by
> RFC 9420 itself, not by Lakko. Layer 3 remains necessary for the by-**value**
> arm, where it is ADR-0022's original committer rule, and for the proposal kinds
> ADR-0045 adds. The module documentation on
> `crates/lakko-mls/src/group_proposals.rs` carries this correction too.

> **Correction from the build: layer 2's stated justification does not exist.**
> This ADR justifies the sweep filter by "a proposal stored while its sender was
> an admin and swept after that sender was demoted". **That window is not
> reachable.** OpenMLS empties the entire proposal store on every merged commit,
> and the role table can only change *by* a merged commit, so the table cannot
> move underneath a stored proposal. Layer 2 is kept, and is worth keeping, but as
> **defence in depth over the store's other writers** — the store has three (the
> receipt gate, this crate's own `propose_*`, and `MlsGroup::load` restoring
> persisted entries) — and it is tested against a store primed by a path that
> bypassed layer 1.

#### Storing a proposal is not free: it muted the whole group

**The consequence this ADR did not record, found only when a test finally sent a
message while a proposal was pending** (`fe3de6c`). OpenMLS refuses
`MlsGroup::create_message` outright while the proposal store is non-empty
(`openmls-0.8.1/src/group/mls_group/application.rs:27-31`,
`MlsGroupStateError::PendingProposal`), on the assumption that a caller commits or
clears the store promptly. **ADR-0045 breaks that assumption deliberately:**
decision 2 retains an authorised proposal until a designated committer sweeps it,
and has no deadline, so a request may legitimately wait hours. Untreated, that
takes the whole group's **chat** down from the moment one admin proposes until the
sweep — every member, not only the proposer. Receiving was unaffected, which is
what would have kept it from being obvious.

**The obvious fix is wrong, and the reason is the second missing fact.** Storing
only at *sweep* time would mute nobody and brick everybody:
`PublicGroup::stage_commit` resolves a commit's by-reference proposals through the
proposal store via `ProposalQueue::from_committed_proposals`, and **that runs on
every receiving member at commit receipt**, not only on the committer at sweep
time. So the store must be populated **on every member for exactly the interval
that fix proposed to keep it empty**, or the commit fails to apply with
`StageCommitError::MissingProposal`. This is what makes ADR-0045's "store at
receipt" **mandatory rather than merely tidy**, and it is also the mechanism
behind the layer-1 correction above.

**What landed instead** touches one path that advances nothing but the sender
ratchet: `Group::create_application_message` **parks** the pending set around the
encrypt call — clone, clear, encrypt, restore in original order, same
`ProposalRef`s — with a byte-for-byte unchanged fast path when nothing is pending.
The pending set does not move house: it stays in the OpenMLS storage provider,
which is what `Identity::snapshot_storage` already serialises and `MlsGroup::load`
already rebuilds, so no persistence is invented and no client-side store is needed.

**Client-visible consequences, all of them nil by design.** ~~The park is a single
synchronous encrypt with no I/O, no `await` and no re-entry~~, so nothing can
observe the window in which the store is empty; the unpark runs
before either outcome propagates and a failed unpark beats a successful send
(losing a pending proposal would desync this device from every peer still holding
it, whereas a refused send costs one message the caller retries). `DeviceGroup`
inherits the fix by delegation and always takes the fast path; `PublisherGroup`
sends no application messages. ~~**S4-S6 need do nothing about this**~~ — **the
clients inherited the unmute with no work, as predicted, but the prediction was
narrower than it read**: what they inherited was *this* fix, and the send path
turned out to need one of its own. The staged sweep gates on `is_active`, not
`is_operational`, so a send is not blocked by a pending commit either — which means
it can be sealed at epoch N while the sweep's commit is already on the wire, and
`lakko-mls` never sets `max_past_epochs`, so a straddling message is not late but
permanently undecryptable. That is a **client** obligation and it is recorded in
[§ What the client sprints found](#what-the-client-sprints-found), item 3. The park
itself is
recorded here because the ADR's decision-3 discussion otherwise reads as if
storing a proposal were free, and because the by-reference-at-receipt constraint
above is a rule a client refactor could break.

> **The struck clause was the load-bearing safety argument for the park, and it
> was wrong.** It is left visible rather than deleted, because it is the reason
> the first landing shipped a defect and because the shape of the mistake is
> worth keeping. The argument — repeated in the `create_application_message`
> doc comment as *"nothing inside it can take a snapshot … this method holds
> `&mut self` throughout"* — reasoned from an exclusive borrow of the **group**
> to the absence of a concurrent reader. **That does not follow.** The `&mut` is
> on `Group`; `Identity::snapshot_storage` takes `&self` on the **`Identity`**,
> a different object, and reads the provider's map under its own `RwLock`.
> Nothing about an exclusive borrow of one prevents a call on the other, and on
> the natives the two genuinely run at once: in `crates/lakko-ffi-uniffi/src/lib.rs`
> the FFI `Group` is `inner: Mutex<MlsGroup>` while the FFI `Identity` is a bare
> `inner: MlsIdentity` with **no mutex at all**, which is exactly the concurrency
> the iOS and Android send-and-persist paths drive. The `lakko-security` review
> reproduced it by execution: **23166 of 25108 concurrent snapshots captured a
> torn pending set**, some capturing zero — and a client that persists one can
> never afterwards apply the by-reference commit those proposals belong to
> (`InvalidCommit(MissingProposal)`), permanently, silently, with no adversary
> involved.
>
> **What actually makes the park safe (`a90fa90`) is that the write was removed,
> not synchronised.** The check OpenMLS makes is `proposal_store().is_empty()` —
> a read of the **in-memory** store and nothing else; persistence has no say in
> it. Both mutators (`clear_pending_proposals`, `store_pending_proposal`) are
> generic over the storage they are handed and each does two things: mutate the
> in-memory store, and mirror that into that storage. The park now hands them a
> **throwaway `MemoryStorage`**, which takes the first effect and discards the
> second, so the real provider's proposal queue is **not written at any point in
> the method**. A concurrent snapshot taken at any instant during a send
> therefore observes the set exactly as it stood before the send, which is also
> exactly how it stands after. No lock, no new field, no cross-thread
> coordination — and, unlike the struck argument, no dependency on what else the
> caller happens to be doing. Pinned by
> `a_snapshot_taken_during_a_send_never_observes_a_torn_pending_set`, which
> drives 400 sends against a real concurrent snapshot thread and restores every
> sample into a fresh `Identity`.
>
> **The general lesson, since it is not confined to this method:** in this
> codebase an exclusive borrow of a `Group` is not a concurrency argument,
> because the persistence surface hangs off `Identity`. Any future reasoning of
> the form *"`&mut self` means nothing else can look"* is unsound here.

#### Storing a proposal was not free for the *old* paths either: a direct commit absorbed it

**The second consequence this ADR did not record, and the one the
`lakko-security` review rated HIGH** (`a90fa90`). Every OpenMLS entry point the
surviving direct-commit paths are built on — `add_members`, `remove_members`,
`self_update`, `update_group_context_extensions` — constructs its commit with
`consume_proposal_store` left at its default `true` and the proposal filter
hardcoded to `|_| true` (`openmls-0.8.1` `group/mls_group/{membership,updates}.rs`).
Each therefore sweeps the **entire** pending set alongside the one change it was
asked for. That was harmless for exactly as long as the set was always empty —
**decision 3's receipt gate created the set, and the paths that predate it know
nothing about it.** Two failure modes followed, and both were live:

- **A silent fork, which is this ADR's own hazard through the back door.** An
  owner proposes an ownership transfer; an admin concurrently calls the ordinary
  `remove_member` on the transfer's target; the admin's commit **absorbs** the
  proposal, produces a commit that `owner_survives` (**C-63**) refuses on every
  peer — and because the direct paths **merge eagerly**, its author is already at
  N+1 and sits there alone, permanently. That is the G-38 signature verbatim.
- **A member with no Welcome, benign-looking and worse than it reads.** An admin
  asking to add one member adds **two**: an `add_member` that also sweeps in
  somebody else's pending `Add` gives that person a leaf on **every peer's**
  roster, while the caller routes the single Welcome to the one person it meant
  to add. The group then renders a member who can never join — they hold no
  Welcome, so they have no group state — and whom only an admin can remove.

**A direct commit is therefore refused outright while anything is pending**
(`Group::refuse_if_proposals_pending`, guarding all five Lakko entry points:
`add_member`, `remove_member`, `self_update`, `set_group_context_extensions` —
and through it `set_group_roles` and `transfer_ownership` — and
`approve_external_join`).

> **Why not the obvious alternative.** The natural fix is to route the direct
> paths through a filtered `CommitBuilder` that **excludes** the pending set.
> That was checked against openmls and **refuted**: merging any commit empties
> the proposal store on every member unconditionally (`staged_commit.rs`, both
> merge arms), *regardless of what `consume_proposal_store` said*. So an
> excluding commit would still **discard** the pending set group-wide — trading a
> silent absorb for a silent loss, which is not an improvement. Refusing is also
> what this ADR's own [no-dual-path rule](#no-migration-and-no-compatibility-path)
> says out loud: a group that has moved to proposals should sweep.

**Nothing that legitimately uses a direct commit today is affected**, and that is
structural rather than lucky: the pending set is only ever written for a group
carrying an ADR-0022 role table (the receipt gate returns early without storing
when there is none, and every `propose_*` refuses without one), so `DeviceGroup`,
`PublisherGroup`, call groups and every other `create_with_id` group have a
permanently empty set and the guard is a counted-zero no-op for them. **What it
meant for the clients** is that in a role-table group the direct paths and the
sweep path are mutually exclusive at any instant — a client that still calls
`self_update` while a proposal is pending gets an error, not a widened commit.
~~**That is now a live consequence rather than a prediction, and it is the sharpest
argument for finishing S7:** `approve_external_join` is still a direct commit on all
three clients~~ — **and it was exactly that, until S7 (`576ea14`) closed it.** For the
window between `a90fa90` and S7, an admin approving a join while any membership
request was pending got a refusal — correct, fail-closed, and a user-visible dead end
nobody designed, which is why it is kept here: the guard behaved as specified and the
dead end came from the *other* path not having converged yet. `approve_external_join`
now publishes a proposal, so `refuse_if_proposals_pending` no longer reaches it at all
and the guard's remaining callers are the paths that genuinely still commit directly.
Pinned by
`a_direct_commit_refuses_to_absorb_a_pending_proposal`,
`a_direct_add_cannot_smuggle_in_a_second_member`,
`a_group_with_no_role_table_still_direct_commits`, and a permanent
`#[cfg(test)]` control,
`control_a_direct_commit_that_absorbs_a_pending_proposal_forks_the_committer`,
which reproduces the hazard through a deliberately ungated adversary path so the
guard is shown to discriminate rather than merely asserted.

#### The pending set is bounded: a cap of 64 and a `ProposalRef` dedup

**Rated MEDIUM by the same review, and reachable with the stock API by a plain
member.** `authorise` deliberately left a plain member two kinds it could emit
without any role: removing its **own** leaf (that is how leaving works) and
`Update` (RFC 9420 §12.1.2 replaces only the sender's own leaf node, so — the
reasoning went — it confers no authority). Nothing stopped one member emitting the
same departure N times: the ratchet refuses only a byte-identical replay, while N fresh
encryptions of one plaintext proposal all decrypt to the same `ProposalRef`, and
`ProposalStore::add` is a bare push with no de-duplication of its own. Since
decision 2 gives a request **no deadline**, the set is otherwise bounded by
nothing but a committer eventually noticing — and the park above makes its size a
per-message cost paid by **every member**, not only by the proposer.

**The `Update` half of that sentence was the hole, and this paragraph is where it
was written down.** Two things it did not consider, both found by the security
review and recorded as items 10 and 11 of
[§ What the security review found](#what-the-security-review-found). First,
"confers no authority" is true of the *proposal type* and false of its *effect*:
openmls does not bind an Update's leaf node to the sender's existing signature key,
so a member can rotate its own key out from under the role table, and
`commit_effects` discards `Update` entirely so `owner_survives` never sees it.
Second, the bound below does not bind it — the dedup collapses a **byte-identical**
repeat, and every Update carries a fresh encryption key, hence a distinct
`ProposalRef`. So the one kind this paragraph named as harmless was the one kind
the cap could not hold. `Update` is now refused outright in `authorise`, which
costs nothing because Lakko emits none.

The same reasoning under-read `Remove`, in the other direction: the dedup does
collapse repeats of one target, but one admin can name every *other* member, and
those are distinct content. So the cap is reachable by `Remove` and by `Add` as
well — see item 11 for why reaching it mattered far more than this section assumed.

Two bounds, mirroring ADR-0027's sibling `MAX_PENDING_EXTERNAL_JOINS` queue:

- **Dedup by `ProposalRef`** in the receipt gate. A repeat of a proposal the
  group already holds is reported `stored: true` — because it is: the group holds
  this exact request, and the proposer's "did it land" answer is yes.
- **A cap of 64** (`MAX_PENDING_PROPOSALS`), applied on **both** sides — the
  receipt gate and this device's own `propose_*`. Both, because an honest device
  must not publish a proposal its peers' gates would refuse for being over the
  cap; it would then hold a proposal nobody else does and sweep a commit they
  cannot resolve.

**At the cap the late proposer pays, never the group.** A proposal arriving into
a full set is **refused** (`stored: false`) rather than evicting an incumbent.
The direction is not a preference: a commit that references an evicted proposal
can no longer be resolved and is refused with `InvalidCommit(MissingProposal)` on
**the evicting member alone** — so refusing the newcomer costs it a re-proposal
after the next sweep, while evicting an incumbent would cost the whole group its
next commit.

> **The honest bound: the cap ceilings the quadratic park cost, it does not
> remove it.** At 64 pending, the clone-clear-encrypt-restore round trip is still
> a per-outgoing-message cost for every member of the group, in the single-digit
> milliseconds. **The dedup is what keeps the realistic count small**; the cap is
> only what stops the pathological one being unbounded. A future arc that wants
> the cost gone needs a different park, not a smaller number.

Pinned by `repeating_one_proposal_does_not_grow_the_pending_set` and
`the_pending_set_is_capped_and_the_late_proposer_pays`.

#### `set_group_roles` no longer self-forks where its twin refuses

**Rated LOW, and an asymmetry rather than a new hazard.**
`transfer_ownership` has validated up front since it landed
(`plan_ownership_transfer`), while `set_group_roles` — the path transfer is
*built on*, and the one a client actually calls to promote or demote an admin —
took any table the caller handed it and **merged eagerly**. So the two disagreed
about the same commit: a table naming an owner no leaf holds was refused by name
through one door and, through the other, built, merged locally, published, and
then refused by every peer's `owner_survives` — leaving its author alone at N+1.
`refuse_unenforceable_role_table` now runs the peer's checks locally **before**
any MLS state is touched, against the **pre-merge** table (ADR-0022 Finding 2, so
an ownership transfer still passes: the transferor is still the owner at the
moment it mints one). The peer gate remains the enforcement; this only stops the
caller self-forking on a commit it could have been told about. Pinned by
`set_group_roles_refuses_locally_what_every_peer_would_refuse`.

> **This was not free, contrary to how it reads.** Four existing tests used
> `set_group_roles` as their *adversary* seam — the way they built a commit no
> peer should accept. Each was migrated to build through the raw extension path
> instead, rather than weakening the fix to keep them compiling. Every
> peer-gate assertion is unchanged.

**Two consequences of decision 3 that are easy to miss.**

- **The `Sender::NewMemberProposal` arm must never be swept implicitly.** An
  external-join Add arrives with sender `NewMemberProposal` — a non-member, so the
  role table cannot resolve it, so a role-based filter that only checks "is this
  sender an owner or admin" would *drop* it, and a filter that passes anything it
  cannot classify would **auto-admit any stranger holding a `/g/` link**, silently
  bypassing ADR-0027's entire approval gate. Neither default is acceptable, so it
  needs its own explicit arm — decision 4.
- **`Proposal::SelfRemove` becomes reachable for the first time, and ADR-0022 said
  so in advance.** ADR-0022 dropped the gate's self-Remove branch as unreachable
  because OpenMLS's `CannotRemoveSelf` forbids a member from committing the removal
  of its own leaf, and recorded the condition under which that stops holding:
  *"If a future sprint makes 'leaving' a controlled action, the gate MUST add
  SelfRemove detection"* (ADR-0022, post-code Finding 1). `authorise_role_commit`'s
  own doc-comment names the mechanism: *"a member leaves by … a self-remove
  proposal an admin commits."* **Option B is that sprint.** It unblocks member
  self-leave — a feature the product does not have today — and simultaneously
  re-opens the branch ADR-0022 deleted. Detecting removals only via
  `staged.remove_proposals()` will not see a `SelfRemove` (proposal type `0x000a`).

**Property tests this needs** (in `crates/lakko-mls/tests/group_admin_roles.rs`,
extending the existing seven, and following the ADR-0044 discipline that a neutered
gate must fail a *named* test):

- `member_proposal_from_a_plain_member_is_not_stored`
- `member_proposal_from_an_admin_is_stored`
- `sweep_excludes_a_stored_proposal_whose_sender_was_since_demoted`
- `sweep_never_implicitly_includes_an_external_join_add` — the ADR-0027 bypass;
  the most important test in the list
- `peer_refuses_a_commit_carrying_a_proposal_from_an_unauthorised_sender` — the
  boundary; must be written against a committer that deliberately skips layers 1
  and 2
- `peer_accepts_a_commit_carrying_only_authorised_proposals`
- `a_by_value_proposal_is_still_judged_against_the_committer`
- `owner_removal_is_refused_even_when_proposed_by_an_admin`
- `role_table_change_proposal_is_owner_only`
- `self_remove_proposal_is_authorised_only_for_its_own_sender`

No new fuzz target: the wire parser is unchanged (`mls_message` already covers the
framing), and the gate is logic over already-parsed structures. The
`role_table_decode` target stands as-is.

> **Where they actually landed, since the list above names a file they are not
> in.** The gate tests live in a new binary,
> `crates/lakko-mls/tests/group_proposals.rs` (22 tests), rather than extending
> `group_admin_roles.rs` (which keeps its original seven, unchanged); the
> ownership-transfer and `owner_survives` tests are a second new binary,
> `group_ownership_transfer.rs` (12 tests). Names drifted from the list — e.g.
> `plain_member_proposal_is_dropped_and_never_reaches_a_commit` and
> `sweep_never_includes_an_external_join_add` — and the built set additionally
> covers what this ADR could not have prescribed: the concurrent-sweep rollback
> loop, two **control** tests that prove the discrimination by reproducing the
> hazard (`control_two_concurrent_direct_commits_still_fork`,
> `control_a_lost_sweep_that_merges_instead_of_rolling_back_still_forks`), and
> four for the mute (`a_pending_proposal_does_not_mute_the_group` and siblings).
> `sweep_excludes_a_stored_proposal_whose_sender_was_since_demoted` is the one
> item with no direct counterpart — see the layer-2 correction above for why the
> window it describes is not reachable. The three layers are additionally tested
> **in isolation** as `#[cfg(test)]` lib tests inside
> `crates/lakko-mls/src/group.rs`, each against a deliberately ungated
> test-only path that skips the layer under test
> (`sweep_filter_excludes_an_unauthorised_proposal_the_store_already_holds` for
> layer 2, `peer_refuses_a_commit_carrying_a_proposal_from_an_unauthorised_sender`
> for layer 3) — which is the ADR-0044 discipline of proving a gate discriminates
> rather than asserting it.

### Decision 4 — external joins already work this way

**How much of B already exists there: the propose-and-approve *shape*, but none of
the mechanism.** ADR-0027 has a non-member propose and an admin approve, which is
structurally B. Underneath, it shares nothing B can reuse:

- The proposal is **not** in the openmls proposal store. `retain_external_join`
  (`crates/lakko-mls/src/group.rs:496`) copies the `KeyPackage` out and pushes a
  `PendingExternalJoin` onto a **Lakko-side in-memory `Vec`**, capped and deduped by
  signature key.
- The approve **does not commit the referenced proposal**. `approve_external_join`
  removes the entry and calls `add_member(identity, pending.key_package)` — a fresh
  **by-value** Add. The joiner's actual proposal is never committed by anyone.
- ADR-0027's recorded reason for this was that `commit_to_pending_proposals` sweeps
  everything and cannot target one joiner. **That reason no longer holds** —
  `CommitBuilder::build`'s filter closure targets precisely.

**Recommendation: subsume, but stage it — and keep the external-join arm explicitly
separate inside the gate.**

- **Subsume the storage and the sweep.** Store the external-join proposal in the
  openmls proposal store like any other, and let the committer's `CommitBuilder`
  sweep it. This retires the parallel `pending_external_joins` `Vec`, the epoch-bound
  in-memory retention the recon flagged as a problem for Option C's retry, and the
  by-value re-add.
- **Do not subsume the authorisation.** An external-join Add is authorised by an
  **explicit admin decision**, never by the sender's role (it has none — it is a
  non-member). So the sweep filter has two arms: *member proposals*, admitted by
  role; *external-join proposals*, admitted only if an admin has explicitly marked
  that one approved. **The default for an unmarked external-join proposal is
  exclude.** `approve_external_join` becomes "mark this proposal for the next
  sweep" rather than "add this member now", and `reject_external_join` becomes
  "drop it" as it already is.
- **Stage it second.** Land the member-proposal path first and migrate the
  external-join path in a following sprint, keeping `approveExternalJoin` /
  `rejectExternalJoin` / `pendingExternalJoins` as FFI names so the three clients'
  approve buttons are not rewritten in the same sprint as the core change. Both FFI
  surfaces already expose all three on `Group` **and** on `PublisherGroup`
  (`crates/lakko-ffi-uniffi/src/lib.rs`), and the members-panel Approve/Reject
  sections on all three clients call them directly.
- **The convergence is a real simplification, not just tidiness.** After it, there
  is exactly one place a membership change enters a group and exactly one gate over
  it, instead of two paths with two retention mechanisms and two authorisation
  stories.

**S7 LANDED in `576ea14`, and staging it second turned out to cost more than "keep
the approve buttons out of one sprint".** For the interval in which the member paths
were converged and the external-join path was not, a role-table group had one live
direct-commit path left, and `refuse_if_proposals_pending` made the two **mutually
exclusive at any instant** — so an admin approving a join while any membership
request was pending was refused. That was the correct fail-closed answer and it was
also a user-visible dead end nobody designed. **G-38 residual 1 is now closed for
this path too**: two admins approving two different joiners against the same epoch
each author an `Add` **proposal**, which advances no epoch, so the two compose into
one sweep instead of forking.

**What S7 built is not what this decision prescribed**, and the three reasons the
prescription does not survive contact are recorded separately, because the design
below is the obvious one and a later hand will re-propose it — see
[§ Why decision 4's design does not survive](#why-decision-4s-design-does-not-survive).
In short: the approval **mark** is device-local while the sweeper may be a different
admin; every peer's commit gate would refuse a stranger's proposal, since a
`NewMemberProposal` sender resolves to no member; and merging the two stores merges
their **quotas**, handing any `/g/` link-holder the membership-freeze primitive C-66
had just closed. The approving admin therefore **authors the `Add` itself**, and
`pending_external_joins` is **kept** rather than retired.

### Decision 5 — what the user sees

**Naming the states only. The screens are `ux-ui`'s call, and per the repo's
UX-first rule that work comes before the core sprints, not after.**

The change is real: "Remove Bob" stops being an immediate local effect and becomes
a request whose effect lands when the committer sweeps. The states a proposing
member's membership action can be in:

- **proposed** — published, not yet swept. The interval is normally one commit
  round-trip with the owner online, so this state is usually momentary; it must be
  designed anyway, because it is not momentary when the owner is not there.
- **applied** — the sweeping commit landed and was accepted. Terminal.
- **superseded** — the epoch advanced without this proposal (it missed the sweep,
  or the group moved for another reason). Auto re-proposed; the user should not
  have to know. Worth surfacing only if it repeats.
- **refused** — the proposer's role changed between proposing and the sweep, so the
  gate excluded it. Terminal, and it needs a real message, because this is the one
  outcome the user cannot fix by waiting.
- **waiting for an admin** — no candidate has swept it yet. **One state, not two.**
  The revised decision 2 has no deadline, so there is no threshold at which waiting
  becomes a distinct *stalled* state and no terminal failure for a request that has
  simply not been picked up; the request waits until a candidate runs the app. The
  honest label is about the *group*, not about a named person's presence — even
  under decision 2's variant 1, a candidate beacon licenses "someone is active", not
  "this person is online", and the client must not imply the stronger claim. This is
  the genuinely new user-visible state B introduces, and the one that most needs
  wording that does not blame the user.

Plus one committer-side surface: **"N changes waiting to apply"**. Recommendation:
the sweep is **automatic**, not a button — a manual "apply changes" affordance would
turn an availability property into a chore and would make waiting more likely, not
less. The pending list itself already has a home on all three clients:
the ADR-0027 members-panel pending-join section (`MembersSheet` on iOS and Android,
the inline `aria-label="Members"` panel in `apps/web/src/routes/Chat.tsx`), whose
Approve/Reject shape generalises to pending proposals with no new navigation.

---

## No migration, and no compatibility path

**Lakko has no production deployment and no users.** There is nothing in the field
to be compatible with, no stored group to migrate, and no mixed-version fleet to
survive. **Option B is a flag day**, exactly as ADR-0044's `push_routing_secret` v2
bump was and for the same reason.

Stated plainly so that nobody builds one: **do not implement a dual path.** No
"legacy direct-commit fallback for groups without a committer", no version
negotiation, no capability advertisement for proposal-based membership, no
"accept a direct membership commit if the sender is an admin" compatibility arm.
Every one of those would be a second, ungated route into the group's membership —
which is to say, an exact reconstruction of the hazard this ADR closes, kept alive
under a name that sounds prudent.

The one genuine compatibility question is internal and answered above: a group with
**no role table** (legacy / pre-ADR-0022) is *open* under today's gate. Under B such
a group has no owner, therefore no rank 0, therefore no committer. Recommendation:
**a group with no role table keeps direct commits**, unchanged, because there is
nothing for B to elect and no gate for it to enforce — and then observe that
`Group::create` has seeded a role table since ADR-0022, so this is a path no
freshly created group can reach.

---

## What Option B does not fix

The recon was honest about Option C's limits; B's deserve the same treatment.

1. **The race is narrowed to the committer handoff, not eliminated.** Two
   candidates that both conclude they are the acting one within a publish
   round-trip can both commit, and one forks. Neither of decision 2's variants
   closes this: a stagger can be outrun by a slow publish, and a heartbeat only
   makes the active set *approximate* — each candidate computes over its own view,
   and those views differ. This is why decision 2 keeps the stand-down re-read and
   a bounded rollback in both. **B is not correct-by-construction; it is correct in
   the steady state and repaired at the edge.**
2. **Membership can wait indefinitely.** Owner offline and no admins, or an
   abandoned owner with no admins ever appointed, means no membership change until
   someone returns. B trades a silent fork for a **visible, honest wait** — the
   right trade, and still a new behaviour that did not exist before. It is a wait
   and not a failure: the revised decision 2 has no deadline and no retry budget, so
   nothing expires and nothing is declared unachievable.

   **ADR-0022's deferred ownership transfer is now built (`e780952`), and this
   item survives it.** Transfer removes the *dead-end in the core* — an owner is no
   longer bound permanently to every group they create, and a member may now
   leave — but it does not remove the *stall*, and neither feature has reached
   every client ~~(transfer has no affordance anywhere; `Leave group` is web-only)~~ —
   **both have since reached all three** (`0274fa7` for the transfer flow, `c50bdd7`
   for `Leave group` on the natives), which changes nothing about this item.
   Minting a transfer needs the owner's key, so it
   is something a present owner does deliberately; it does nothing for an owner
   who has already gone, which is precisely the case that stalls. The remedies for
   a stall remain what they were: appoint more admins, or have the owner hand the
   group on **before** they stop running the app. Recovering a lost owner is
   ADR-0022's separate, still-open admin-quorum question.
3. **A new authority shift: the committer can selectively censor.** An elected
   committer that sweeps some proposals and drops others is not detectable except by
   each proposer noticing its own proposal never landed. Today an admin acts
   unilaterally and cannot be filtered by a peer. Bounded by the fact that another
   candidate becomes the acting committer once this one stops acting — so a
   committer that sweeps *nothing* loses the duty — but **partial** censorship by an
   otherwise-active committer is not bounded by anything in this design. Named, not
   solved.
4. **Proposals go stale too, and the automatic re-proposal on *supersession* is
   still not built — but the silence around it is closed, and the sibling case
   now retries by itself.** ValSem003 rejects *any* non-Application message at the
   wrong epoch, not only Commits, so a proposal that misses the sweep must be
   re-proposed. This is cheap — the whole point — but it is a client state machine,
   not a no-op, and a stale proposal litters the journal for its 30-day TTL the way
   the recon says a dead commit does under C. ~~**As built, none of the three clients
   re-proposes.**~~ **Three things have changed under this item and they pull in
   different directions, so it is worth separating them:**
   - **Still unbuilt:** the UX spec §7 *superseded* case — a silent, automatic
     re-request of a proposal the sweep dropped. No client has it. **Now required
     (2026-10-01)** by [ADR-0050](0050-per-device-conversation-leaves.md) section
     2.5a, under which a plain-member leader sweeps only device admissions and
     every other pending proposal is dropped by its merge; the rules it is built
     to (trigger on an observed commit, re-derive from intent, stop by state) are
     recorded there.
   - **No longer silent (`46c7aa4`):** an author now learns that a request of theirs
     left the pending set without its effect landing — the `Not applied (n)` surface,
     with a user-driven repair. It is scoped to **adds**, and that is a rule rather
     than a shortcut: `remove` and `leave` key on a **leaf index** and OpenMLS reuses
     a vacated leaf, so a roster test on one is wrong in both directions, and all
     three are already visible when they fail.
   - **Built for the sibling case (`21f6c8c`, `1839da7`):** a request refused because
     the queue was **full** re-sends automatically on all three, triggered by an
     observed shrink of the pending set rather than by a timer — because a queue
     drains when a committer sweeps, not when time passes.

   So the residual is narrower than it was and it is still real: a **superseded**
   request is dropped and the user must ask again, now knowing that they must. It
   remains a functional gap rather than a safety one.
5. **`self_update` / `PublisherGroup::rotate` is a fifth epoch-advancing path B
   does not cover.** `rotate` (`crates/lakko-mls/src/publisher_group.rs:455`) is
   FFI-exposed on both surfaces and advances the epoch via `self_update`. Two
   ADR-0028 OQ3 co-publishers rotating concurrently reproduce this ADR's hazard on a
   `PublisherGroup`, and B — scoped to conversation-group membership — does not
   touch it.
6. **The web two-tab case is not fixed and cannot be by this design.** G-38
   residual 2 records that two tabs on one account each hold an independent wasm
   provider and clobber each other through an `accountId`-keyed snapshot row. Under
   B both tabs derive the **same** signature key and therefore the **same rank**, so
   the deterministic ordering cannot separate them. ADR-0044's cross-tab Web Lock
   remains the only mitigation, and it is secure-context-only.
7. **DeviceGroups and call groups are out of scope, for different reasons.** A
   DeviceGroup has no ADR-0022 role table (it carries the ADR-0015 removal policy
   instead) and its members are one user's own devices, so it has neither the
   authorisation input B needs nor the multi-admin concurrency shape B targets. A
   **call** group carries a role table but ADR-0022's post-code Finding 2 requires
   clients to route call-group receives through the **open** `process_incoming`, not
   the verifying gate — so call groups keep ADR-0032's presence-based
   `elect_committer` and are untouched. The two election functions coexist by
   design; neither subsumes the other.
8. **It does not make an already-forked device converge.** Nothing in B repairs a
   device that forked before B shipped. With no deployment that is vacuous today,
   and it stops being vacuous the moment there is one.
9. ~~**The external-join path is still a direct commit, so G-38 residual 1 is closed
   for the member paths and open for that one.**~~ **Closed by S7 (`576ea14`).**
   `Group::approve_external_join` no longer calls `add_member`: it publishes an `Add`
   **proposal the approving admin authors itself**, carrying the joiner's retained
   KeyPackage, so two admins approving two different joiners against the same epoch
   compose into one sweep instead of forking. The question this item said needed
   deciding — *what `Approve` means once it stops meaning "added"* — was answered by
   the build: it means *the admin's own MLS-authenticated proposal is on the wire*,
   which is why S7 needed no new authorisation channel. The one direct external-join
   arm that survives is `pub(crate) approve_external_join_directly`, reachable only
   from `PublisherGroup::approve_external_join` and refusing any group that carries a
   role table.
10. **A send can still straddle an epoch when the commit is a REMOTE one.** The
    client sprints closed the local half by serializing seal-and-publish under the
    per-group lock, and the bound is written into the code rather than implied: a
    message sealed on this device and published after a **peer's** commit has
    already been ordered ahead of it is still permanently undecryptable, because
    `lakko-mls` never sets `max_past_epochs` and openmls's default of `0` retains no
    past-epoch secrets. No client-side lock reaches that, for the same structural
    reason no lock reaches the fork this ADR is about — the other party is a
    different device. Closing it means retaining at least one past epoch, which is
    an invariant-#3 change with its own ADR and property tests, and it is a real
    trade rather than an oversight: retention is a forward-secrecy cost.

---

## What the client sprints found

Eight things the clients found that this ADR did not anticipate, and one it got
wrong in the dangerous direction. They are here rather than in commit messages
because each is a rule a later hand can break, and because the pattern across the
first six is worth naming: **every one is a consequence of a membership operation
ceasing to be atomic.** The direct path advanced the epoch inside one call; the
proposal path splits it into propose, sweep and adjudicate, and each of those six is
something that used to sit inside that atomic step and now spans it.

Items 7 and 8 are a different kind, and how they were found is the point. They were
surfaced not by writing the sweep but by **mirroring it onto a second platform** — a
defect that reads as correct in one implementation becomes visible the moment a
second author has to reproduce its behaviour and cannot. Both were present in the
web reference, both had shipped, and neither was recorded anywhere. That is the
argument for mirroring attentively rather than transliterating.

Items 9 to 15 are a **third** kind: they were found by the `lakko-security` review
this ADR had owed since its first sprint, which returned **REQUEST CHANGES**. They
are grouped separately below, under
[§ What the security review found](#what-the-security-review-found), because the
lesson they carry is not the atomicity one. Three of them are HIGH, one is a
confirmed impersonation — and the thing worth saying about all seven together is
that **not one of them needed an attacker, and four needed nobody at all**: a
network blip, a stale roster, two admins doing the ordinary thing, or simply enough
time passing. A review that had run six sprints earlier would have cost less than
the seven fixes did.

**1. The verdict's open question was wrong, and wrong in the dangerous direction.**
[§ Open questions](#open-questions) asked whether `epoch_of_message` should gain a
`contentTypeOfMessage` sibling, and answered that "epoch alone is a *sufficient*
signal when combined with *this row is not one of our own artifacts*". It is not.
Another member's ordinary application message is framed at N, is not one of our
artifacts, and is not a commit — so epoch-alone reads it as a **loss** and abandons
a sweep that actually **won**, after every peer applied it, leaving this device at N
with the group at N+1 and its own commit un-re-appliable
(`CannotDecryptOwnMessage`). That is G-38's brick reached in reverse, not an
accuracy nicety. What the clients built instead needs no discrimination at all:
**apply every row below our own commit's DS id, then ask whether our epoch moved**
— only a commit moves it, and applying is the audited receive path. That makes the
FFI addition unnecessary for the verdict; it survives as a smaller question about
the stand-down (below).

**2. The stand-down stands down on any unapplied row, not only on a commit.** The
same missing discrimination, met from the other side, and resolved the other way
round — because the costs are asymmetric. Standing down on a row that turns out not
to be a commit costs **one retry**; staging on a row that *was* one costs a commit
every peer drops. So all three clients stand down on any unapplied journal row. This
is where a `contentTypeOfMessage` peek would still earn its keep: it would make the
stand-down exact rather than conservative. It is an optimisation now, not a
correctness gap.

**3. The send path had to be serialized, and the sweep is what makes it certain.**
The clients found that a chat message can be **sealed at one epoch and published
across a commit that changes it**. `lakko-mls` never sets `max_past_epochs`, so
openmls's default of `0` applies and a recipient retains **no** past-epoch secrets:
such a message is not late, it is **permanently undecryptable for every recipient**,
dropped silently while the sender's own echo stays on screen. Both directions are
reachable — *sealed ahead*, when a direct commit region has eager-merged and then
parks in its persist and publish; and *sealed behind*, which is the one this ADR
created. A staged sweep publishes its commit and then spends a round trip reading
the verdict, and `create_message` gates on `is_active`, **not** `is_operational`, so
a pending commit does not block a send. For that whole window the loss is not a race
but a **certainty**. All three clients now hold the per-group MLS lock across
seal-and-publish. **The bound is stated rather than glossed:** this closes the
**local** half only — a message sealed here and published after a *remote* commit
already ordered ahead of it is still lost, and closing that needs `max_past_epochs`
above zero in `lakko-mls`, which is invariant #3 and would need its own ADR and
property tests.

**4. C-61 moves with the removal, and no document said so.** ADR-0043's ordering
rule — publish the Remove commit, *then* rotate the push routing secret — was seamed
onto `removeMemberFromGroup` on all three clients, on the premise that a removal
always lands there. Under ADR-0045 it usually does not: it becomes a **proposal**,
which advances no epoch, so rotating at that point seals the new secret at an epoch
**the member being evicted is still in and can open**. That silently revokes C-61's
forward-secrecy half without touching a line of rotation code, and under ADR-0044 it
also stamps the new secret at the pre-removal epoch, where it **ties** with the
stale secret and can simply lose the byte tiebreak. The rotation therefore moves to
where the removal actually lands — the sweep, via the membership-**shrink** diff the
poll already runs for someone else's removal — which keeps the ordering correct by
construction rather than by an argument about two adjacent statements. Android
reaches the same place from the other end, with a removal-landed flag the rotate
step consults. **The general rule, worth stating because it will recur: any client
discipline seamed onto a direct-commit call site has to be re-seamed onto the sweep,
and the ones that matter are the ones whose correctness argument mentions an
epoch.**

**5. ~~Concurrent role changes became lossy~~ Concurrent role changes wedge membership,
and neither document noticed — then this item understated what it had found.** A role
change is a read-modify-write whose read is the committed role table and whose write
is now a proposal. On the direct path the per-group lock sufficed — the first
toggle's commit updated the table and the second read it. On the proposal path a
proposal changes nothing until it is swept, so two toggles taken a moment apart both
read the **same** committed table and each proposes a table carrying only its own
change. Serializing cannot fix that, because there is nothing for the second read to
observe, and the client cannot repair it either — `ProposalKind::RoleChange` carries
no payload, so it cannot read back what it proposed.

~~RFC 9420 permits at most one `GroupContextExtensions` proposal per commit, so the
sweep keeps one and **silently discards the other**.~~ **Corrected 2026-08-28, and
this item stated the milder of the two faces.** RFC 9420 admits at most one
`GroupContextExtensions` proposal per commit, and OpenMLS enforces that as a **hard
validation failure**, not a silent selection: `validate_group_context_extensions_proposal`
returns `TooManyGCEProposals` on the second one
(`openmls-0.8.1`, `src/group/public_group/validation.rs`), and it is reached from
`CommitBuilder::build` (`src/group/mls_group/commit_builder.rs`). So a pending set
holding two role changes **cannot be swept at all**. The proposal store empties only
when a commit is *merged*, and `refuse_if_proposals_pending` closes every direct-commit
escape (add, remove, role-change, external-join approve, `self_update`) while anything
is pending, so a committer holding both has **no way out**: its membership
operations are dead. **Both faces are real, and this item named only the second.**
Since both proposals travel the journal, the ordinary case is that both reach every
candidate committer — and then **the whole group's membership is wedged**, escapable
only if some committer's view happens to hold just one. The **asymmetric** case is the
one described above: a committer holding only one sweeps, the merge empties everyone's
store, and the other change is dropped group-wide. That silent discard is the *lucky*
outcome, not the failure mode. Chat is unaffected in both, because
`create_application_message` parks the pending set.

**Closed in the core (2026-08-28), in two halves, and neither is redundant.**
`Group::refuse_if_role_change_pending` refuses a second role change at propose time. It
is placed **last** in `propose_group_roles` — after authority (`authorise_own_proposal`)
and validity (`refuse_unenforceable_role_table`) — so a caller handed a broken table
still learns *that* rather than "not now", and it counts only proposals classifying as
`ProposalKind::RoleChange`, so a pending add, remove or leave does not block. `sweep_keeps`
carries the other half: **at most one role change per sweep**, by the same
exclusion-not-refusal mechanism its existing `role_change_survives` clause uses.
**The sweep-side half is not made redundant by the propose-side guard**, and the reason is
recorded here because a later hand will otherwise read it as duplicate and delete it: the
guard cannot see a state that arises **legitimately**. A device that publishes a
role-change proposal and then loses its own record of it before persisting — the ADR-0040
crash window — proposes a second one in good faith, and every *peer* then holds two.
Without the sweep-side bound those peers can never sweep again. The guard keeps a
well-behaved device from building the set; the bound keeps a set that reached it anyway
from wedging the group. Indexed as **C-65**.

**Which sharpens "the client cannot repair it either" in a second way.** The repair has to
be a **refusal before the second proposal exists**, because once it exists no client can do
anything about it: it cannot withdraw a proposal, it cannot read back what it proposed,
and — under the corrected reading — its committer cannot even sweep past it. That is the
argument for putting the rule in the core rather than in three clients.

~~Web and iOS **refuse** a second role change while one is pending and say so; **Android
does not**, so a second concurrent toggle there still loses one silently.~~ **Stale as of
the core fix.** The refusal now holds for **every** caller on every platform and cannot
drift between them. ~~What Android still lacks is **readable copy for the refusal**, not
the refusal itself: it will surface the core's `Error::Group` diagnostic where web and
iOS surface a sentence.~~ **Closed 2026-09-01 in `8018e8b`, and the fix went further than
this follow-up asked.** Android gained the same narrow `hasPendingRoleChange` pre-check
web and iOS already had *and* the sentence — but the drift this bullet describes was
three-way, not one-way (web surfaced the spec's string, iOS threw its own near-variant
prose, Android surfaced the core diagnostic raw), so mapping core prose to fixed copy by
substring would have rebuilt the same drift class. The core now raises
`Error::MembershipRefused { reason, detail }` (`crates/lakko-mls/src/refusal.rs`), whose
whole `Display` is `detail` — the same sentence the refusal always carried, so nothing
about a log line changed — with a typed `RefusalReason` travelling **beside** it as the
contract. Each client selects its copy from the reason rather than from the text.

The UX spec's "a second, different add composes into the same sweep" is true for adds and
**false if generalised to role changes**.

**6. The direct-commit refusal reaches the external-join path, and only S7 closes
it. — CLOSED by S7 (`576ea14`); kept because it is the item that argued for S7.**
`refuse_if_proposals_pending` (the `a90fa90` hardening) makes the direct and
sweep paths mutually exclusive in a role-table group. While `approve_external_join`
was still a direct commit on every client, approving a join while any membership
request was pending returned an error. Correct and fail-closed, but a user-visible
dead end nobody designed, and it was the sharpest practical argument for finishing S7.

*The interim mitigation is gone with the cause, and it is recorded because a reader
of the client history will find its remains.* Before S7, approve **drained first** —
it ran the committer sweep, then re-read the pending count **inside** the lock and, if
the set survived, refused in copy a person can act on instead of in a core diagnostic
naming two Rust entry points. Neither native lock is reentrant (iOS's `withCommitLock`
is a task chain that would await the tail its own outer section installed; Android's
is a `kotlinx.coroutines.sync.Mutex`), so the drain ran **outside** the lock on both
and the in-lock re-read closed the window the poll opened between them. Two ceilings
were named as S7's to lift rather than a client's, and S7 lifted both: the stand-down
stands down on **any** unapplied row including this device's own just-published
proposal, so an admin who removed someone and immediately approved a join was still
refused; and an admin correctly **deferring to a live owner** never drained at all, so
for a non-owner admin the Approve button was effectively unavailable whenever anything
was pending. Both are gone because approving no longer needs an empty queue: it
publishes a proposal, and the clients follow it with an opportunistic
`sweepIfCommitter` whose failure is now an honest *they join when an owner or an admin
applies the change* rather than a refusal.

**7. The verdict apply swallowed the rows it applied.** The sibling of item 1, and
the sharper half. `adjudicate` applies every row the DS ordered below its own commit
in order to read the epoch — and each row went through the audited receive path with
its **result discarded** while the shared cursor advanced past it. When such a row is
an ordinary chat message the ratchet advances correctly, the cursor moves on, and the
plaintext is dropped: the message never reaches the timeline, and because the cursor
already passed it, no poll ever re-fetches it. Permanently lost, silently, with no
error anywhere. The window is narrow — the stand-down has just proved the journal
empty — but it is exactly the window a concurrent sender lands in, and it is reached
again on the restart path. **The fix is that the verdict apply must surface what it
applies through the same event ladder the poll uses**, never a second copy of that
dispatch, since two copies drift; the natives split their existing poll-side consumer
so the sweep can dispatch without touching the cursor. Note the ordering trap web hit
and the natives did not: rows are applied *before* the verdict is known, so a client
that returns early on a **loss** swallows them anyway. iOS had this recorded as a
`KNOWN CONSEQUENCE` comment in its own `adjudicate` and had shipped it, which is worse
than not knowing — a defect annotated in place reads as a decision.

**8. A mapping two clients read backwards — on a branch nothing reaches.** Web and
Android resolved a pending request's target straight off `ProposalKind::Remove`'s
target field, which is `None` for a `Leave`: RFC 9420 `SelfRemove` names no target at
all and the removed leaf is the sender's own, exactly as `commit_effects` says. Both
read that absence as *no target* rather than as *the target is the sender*, so their
`leave` branch could never match. iOS resolved it correctly, and the mirror had to
account for the discrepancy, which is the only reason it surfaced.

**The severity was first recorded as "the `Waiting to leave.` copy never renders", and
that is wrong** — the correction is kept rather than quietly edited, because the way it
was got wrong is the instructive part. The copy renders, on the branch that actually
carries a self-leave: `Group::propose_remove_member` always emits
`ProposalKind::Remove { target_leaf }`, so a member leaving proposes the removal of
its **own** leaf and folds as a `remove` whose target is populated, which
`rowAnnotation` then reads as `Waiting to leave.` because the target is the reader.
`ProposalKind::Leave` is **unreachable from the wire in Lakko**: a `SelfRemove` must be
sent as a `PublicMessage`, and `Group::create` keeps the OpenMLS pure-ciphertext
policy, so OpenMLS refuses to craft one — the core pins that refusal in
`self_removal_is_gated_for_its_own_sender_and_refused_for_the_owner` precisely so that
changing the wire-format policy (which would stop encrypting handshakes to the
zero-trust DS) fails a named test and forces a look at this arm. So the mapping was
wrong, the fix is right and is now unit-tested, and **no user ever lost anything by
it**. The lesson is not about the mapping: reading the FFI's shape and the UX spec was
enough to conclude the branch was dead, and it took reading the *core's* wire-format
policy to learn that the branch is dead for an entirely different reason and that the
feature worked all along. **A defect inferred from two layers is a hypothesis until the
third layer is read.**

## What the security review found

The `lakko-security` pass this ADR names as a precondition for its own Accepted
status finally ran against `9a60ab3 → 8763f3e` and returned **REQUEST CHANGES**:
three HIGH findings plus a confirmed impersonation. All seven are closed in
`c50bdd7`, `0274fa7`, `8763f3e` and `5b2b654`. They are recorded here rather than
only in commit messages for the same reason the eight above are — each is a rule a
later hand can break — and with one addition: **several of them were first
mis-triaged, and the mis-triage is part of the record.** A reader who sees only the
conclusion will re-derive the wrong reasoning.

**9. An author was a leaf index, and leaves are reused.** ADR-0023's author-only
gate — only a message's original author may edit or delete it — compared a raw MLS
leaf index, and OpenMLS hands a vacated leaf to the next Add. This ADR is what made
it cheap: a Remove and an Add compose into **one commit**, so the whole sequence
lands in a single poll batch. Remove Bob, add Dave, and Dave rewrites and deletes
Bob's history. On both natives it was permanent, because the leaf was on disk.
Reactions and read-receipt watermarks had the identical shape, and all three
clients independently measured that the reaction damage is worse than a
mis-toggle: removing the last reactor prunes the set, so the chip **disappears**.

The fix is not "compare keys" — it is **compare keys resolved at receipt, in the
epoch the row arrived**. A late resolve reintroduces the impersonation exactly,
because a batch carrying the removal and the add would resolve an epoch-N message
against the N+1 roster; all three clients carry a permanent contrast test that
pins it. **The wider point:** every module built after ADR-0023 — the role table,
the committer rank, the beacon store, the push-routing gate, the shrink diff —
keys on the signature public key, and several name leaf reuse in a comment as the
reason. ADR-0023 predates that convention and was never revisited when the
convention formed. Conventions do not retroactively fix the code written before
them.

**10. `Update` proposals were a hole in two directions at once.** `authorise`
accepted `ProposalKind::Update` from any member and `commit_effects` discarded it,
so [`owner_survives`](#never-leave-the-group-without-an-owner-in-it) could not see
it — and openmls does not bind an Update's leaf node to the sender's *existing*
key, so a member can rotate their own leaf's signature key out from under the role
table. Separately, each Update carries a fresh encryption key and therefore a
distinct `ProposalRef`, which defeats the receipt gate's dedup, so 64 of them fill
the pending set. Both closed by refusing `Update` outright in `authorise`, which
costs nothing: Lakko emits none, and `self_update` — which **is** live, in the
channel rotation path, contrary to the review's own note — drives its rotation
through the UpdatePath rather than a proposal, measured rather than argued.

**11. A full pending set took away the owner's means of response, and calling that
"weaker than what an admin already holds" was wrong.** The cap refused *any*
proposal at 64, including the owner's own `Remove` and `RoleChange`; the direct
paths were already refused by `refuse_if_proposals_pending`; and nothing can
discard a proposal. So a flooding admin locked the owner out of removing them,
demoting them, or clearing the queue — and the only exit, sweeping, **committed the
flooder's 64 adds**, after which they re-flood.

The first triage dismissed this as strictly weaker than a capability the role
already has, since an admin can simply remove everyone. That is the error worth
recording: **capabilities compose, and the composition is not the sum.** Removing
people is loud, attributable and repairable — the owner demotes the admin and adds
them back. Filling the queue removes the *means of repair*, which is a different
thing and strictly stronger. Closed with a small reserve of slots past the cap,
admissible only to a proposal `is_owner_response` recognises: authored by the
owner, kind `Remove` or `RoleChange`. It grants nothing — `authorise` still runs
afterwards, unchanged — it only decides when the set is too full to accept a
request. A per-sender sub-cap was rejected because it delivers a probability where
the property needed is "never": several malicious admins still reach the cap.

**12. An unbuildable sweep is a permanent, invisible, group-wide membership
freeze — and one cause of it is the passage of time.** If `CommitBuilder::build`
hard-fails, it is not a lost round: the proposal store empties only on a *merged*
commit, every direct commit is refused while proposals pend, and nothing discards.
Meanwhile `create_application_message` parks the pending set, so **chat keeps
working and the group looks healthy** while its membership can never change again.

One proposal is enough. An `Add` naming someone already in the group, or two admins
independently inviting the same person — which is precisely the concurrent
composition this ADR exists to make safe — hard-fails with `DuplicateSignatureKey`.
Fifteen build conditions are now enumerated in `sweep_keeps`, and the one that
settles the argument is **KeyPackage expiry**: decision 2 gives a request no
deadline, so a proposal need only wait long enough and the group freezes with
nobody having done anything at all.

**13. The enumeration cannot be the whole answer, so it is backed by an exclusion
search.** Fifteen conditions is a mirror of a rule set this crate does not own; an
openmls upgrade adds a sixteenth and the freeze returns. So when the build still
refuses, `narrow_sweep_by_exclusion` offers candidates one at a time and keeps each
only if the selection still builds, then re-offers the rejects once. **It does not
need to know why a proposal is uncommittable — only which one**, which is what
makes it robust against rules nobody has written yet.

It earned itself immediately, on a case the enumeration does not catch and arguably
should not: **an admin proposing its own departure wedges its own sweep**
(`CannotRemoveSelf`). That is ordinary traffic this ADR introduced — leaving *is*
"the member proposes it and someone else commits it" — and the proposal is not
defective, it is simply not committable *by that device*. Three things the build
found that the design did not: additive selection beats leave-one-out because the
cheap single-offender case is exactly the one you cannot assume when the cause is
unknown; the second pass is load-bearing, because a `Remove` and the re-`Add` of
one key are only committable **together** and one pass silently evicts the member;
and each trial selection must re-ask `owner_survives`, because dropping an `Add`
can strand a role change naming that person owner — which openmls, knowing nothing
of role tables, will build happily and every peer will then refuse, trading a local
freeze for a fork. Cost measured, not asserted: 137 build attempts at the cap, and
215 ms in release on the expensive shape, once.

The search is deterministic per device and **deliberately not** device-independent
— "can I commit this" is a question about the committer, which the departing-admin
case proves. Two committers may therefore stage different subsets; the journal
ordering already arbitrates that, the loser abandons, and since a merged commit
clears the whole store a proposal dropped by either search is dropped everywhere
and its author re-proposes.

**14. The merge is not the end of the sweep's obligations.** A Welcome fan-out that
failed *after* the commit had won and merged was unrecoverable: the next tail sweep
finds no staged sweep and no pending proposals, the restart path finds no owed
verdict, and the legacy loop skips the entry. The added member sits in every
roster, has every message encrypted to their leaf, and can never construct group
state — repairable only by another admin removing and re-adding them.

Obligation 2 as this ADR states it is an **ordering** rule — publish the Welcome
only after the verdict — and that half is right and unchanged. What was missing is
its second half: **record that the verdict settled, before you try.** The general
rule, worth stating because it will recur: an artifact still owed after the
epoch-advancing step needs its own durable name, because the state every recovery
path branches on is gone by the time it is owed. A three-state entry lifecycle
(verdict-owed → welcomes-owed → acked) replaces the two-state one. Note also that
the obvious fix — clearing the verdict flag — would have made two clients **worse**,
handing the entry to a loop that publishes no Welcome and then acks, destroying the
record.

**15. This ADR states the committer's obligations and never states the receiver's.**
That omission is the gap the last finding came through. The arc made a receiver's
*durable proposal store* a precondition for applying a later by-reference commit —
`group_proposals.rs` relies on it as a boundary — but the receive-path snapshot
write was best-effort and swallowed failures, the cursor lives in a **different
store that no transaction can span**, and the poll advanced the cursor before
processing. One swallowed write and the device restores an MLS state without the
proposal and a cursor past it; the commit referencing it throws `MissingProposal`,
is skipped, the cursor advances past that too, and the device sits at epoch N
forever while the group is at N+1. It is still a member, so `is_active()` cannot
see it.

The rule is now that **the journal cursor may lag the durable MLS state, but never
lead it** — persist fail-closed first, advance second, hold on failure. The
asymmetry is what makes it work rather than atomicity: cursor-behind is
self-healing, since re-fetched rows are rejected as wrong-epoch and skipped exactly
as the poll already skips them, while cursor-ahead is permanent. Making the two
stores one was considered and rejected — it is impossible on the natives at any
price and expensive on web, and it would only buy *equality* where ordering already
suffices. ADR-0040 solved this same shape for the committer with an atomic write;
the receiver's answer is an ordering rule, and it is one rule on three platforms
rather than one platform's stronger guarantee.

---

## Why decision 4's design does not survive

Decision 4 prescribed the shape S7 should take: **store the stranger's external `Add`
in the openmls proposal store, and give `sweep_keeps` a second arm that admits it once
an admin has marked it approved**, defaulting to exclude. S7 was built and that design
was rejected on contact. The reasons are recorded because the design is the obvious one
and a later hand will re-propose it.

**1. The mark is device-local; the sweeper may be a different admin.** ADR-0045's whole
point is that the device which *decides* and the device which *commits* are not the same
device — the committer is whichever candidate is lowest-ranked and running. An approval
recorded in one admin's memory is invisible to the one that actually sweeps, so either
the mark has to travel (a new authorisation channel on the wire, which is a bigger change
than the one being avoided) or the sweeper admits an unmarked stranger, which is the
auto-admit ADR-0027's gate exists to prevent.

**2. Every peer's commit gate would refuse the result.** A `NewMemberProposal` sender
resolves to no member leaf, so `authorise` refuses it for **every** kind — that
unresolvable-sender refusal is what keeps a stranger's own proposal out today. A commit
carrying it would be refused by each peer on receipt, and the only evidence available to
relax that refusal is *an admin committed it*, which is precisely the trust in the
committer that the peer-verified gate is built not to extend.

**3. Merging the stores merges the quotas, and hands a stranger a freeze primitive.**
`MAX_PENDING_EXTERNAL_JOINS` is a **quarantine for non-members** — its whole reason for
being separate is that anyone holding a `/g/` link can fill it, and filling it costs the
group nothing because nothing downstream reads it. Move strangers into the member
proposal store and that stops being true: the store is what the sweep consumes, what
`refuse_if_proposals_pending` guards, and what C-66's cap bounds. Any link-holder could
then fill it and freeze the group's membership — the exact denial this ADR's own C-66
work spent a sprint closing, handed to people who are not even members.

**What was built instead, and why it is smaller.** The approving admin **authors the
`Add` itself**. That puts the approval on the wire as an ordinary MLS-authenticated
artifact judged by the one existing rule: the admin's signature *is* the approval, there
is no new authorisation channel, no new sweep arm, and no relaxation of the
unresolvable-sender refusal. On the wire an approved add is **indistinguishable from an
admin adding a contact**, which is the property that makes it sound rather than a
limitation to work around — a sweep that could tell the two apart would leak *this member
arrived via a public link* to every member and to the journal.

Two things fall out that decision 4 did not anticipate. **`pending_external_joins` is not
retired** — decision 4 wanted it gone, and keeping it is what makes reason 3 not bite.
And the API **splits**: `PublisherGroup` is built by `Group::create_with_id`, seeds no
role table, and the proposal path refuses a table-less group by construction, so the
channel co-publisher approve keeps the direct arm. That is consistent with ADR-0045
(a table-less group keeps direct commits) but it means S7 could not retire the old entry
point, only confine it — `Group::approve_external_join_directly` is `pub(crate)`, has one
caller, and refuses a group that carries a role table, so it cannot become a second
ungated route into a group that has a committer.

**The cost that is real and is not hidden.** The sweeping device cannot tell which adds
came from a `/g/` link, so it posts a `WelcomeRendezvous` for **every** added key
alongside the mailbox fan-out it already does. A rendezvous must be readable by a
not-yet-member, so it carries the added member's **raw signature key** on the group
journal where an ordinary add puts only an unkeyed hash of it on a mailbox pseudonym.
For a peer this is no new capability — both are derivable from a public key by anyone
holding it — but the **DS** now reads the key directly instead of a hash whose preimage
it would have to guess. Invariant #5 still holds (the DS learns no account identity), and
the disclosure is stated here rather than left to be discovered.


## Blast radius, per sprint

| Sprint | Surface | What changes |
|---|---|---|
| **S0 — UX** — **LANDED `c5c0788`** | none (design) | The five states of decision 5 plus the committer-side pending surface. `ux-ui` first per the repo rule. Landed as [`docs/ux/proposal-based-membership.md`](../ux/proposal-based-membership.md), out of order — after S1+S2 rather than before them. |
| **S1 — core: the gate** — **LANDED `9a60ab3`** | `crates/lakko-mls` (**invariant #3**) | `process_incoming_verifying_roles` proposal arm: inspect + role-check + `store_pending_proposal` or drop. `authorise_role_commit` extended from committer-only to per-proposal-sender, with the by-value arm preserved and a `SelfRemove` arm added. New `Incoming` shape for a stored-vs-dropped proposal. Ten property tests. **External crypto review — still owed.** As built: the whole rule is `crates/lakko-mls/src/group_proposals.rs` `authorise`, written down once and consumed at all three layers. |
| **S2 — core: the committer** — **LANDED `9a60ab3`** | `crates/lakko-mls` | `committer_rank` in `group_roles.rs`; a sweep entry point over `CommitBuilder::build` with the two-arm filter; `propose_*` wrappers over the openmls calls. As built the sweep is named `commit_pending_proposals`, with `pending_proposals()` and `own_committer_rank()` beside it. |
| **S3 — FFI** — **LANDED `c5c0788`; completed by S3b `6c77cd6`** | `lakko-ffi-uniffi`, `lakko-ffi-wasm` | Add the propose + sweep + rank + pending-proposal-list surface beside the existing `add_member_from_wire` / `remove_member` / `set_group_roles` / `approve_external_join`. Both surfaces, in lock-step. Regenerate bindings, rebuild `LakkoCore.xcframework` and the Android `.so`. ~~S3 landed BEFORE S2b and S2c, and neither was back-filled onto it~~ — **both were, in S3b.** |
| **S4a — web reference** — **LANDED `1de5fe7`** | `apps/web/src` | Admins propose, the **owner** sweeps — no beacon, no election, no takeover, which is correct by construction at rank 0 (nothing to elect with exactly one committer) and leaves an admin-only group waiting, the designed behaviour. `contacts/client.ts`: the four `…Locked` methods become propose-and-publish, gated on `usesProposalPath` so the no-role-table path is byte-for-byte unchanged. New `chat/sweep.ts` (the stage -> publish -> verdict -> merge/abandon orchestration over injected ports, host-testable with no wasm, no IndexedDB and no network) and `chat/pending-requests.ts` (the §4 state fold + the §5 copy). `chat/commit-outbox.ts` gains the sweep entry; `resolveOwedVerdict` is wired into ADR-0040's existing `sweepPendingCommits` seam rather than becoming a second restart path. ~~The retained epoch-N snapshot~~ — none is needed. `routes/Chat.tsx`: the members panel states and `Leave group`. |
| **S4b — the beacon, web** — **LANDED `02f0037`** | `apps/web/src` | Ends the admin-only wait: candidates announce themselves and the lowest-ranked active one sweeps. `chat/committer-beacon.ts` — the emitter's foreground+throttle gate, the per-`(group, signature key)` observation store, and `decideActingCommitter` over the core's fold. **`composeCommitterBeacon` gets its first call site anywhere in the tree.** Ranks are re-derived through `Group.committerRank` **at verdict time**, never carried from receipt; age comes from the DS row, never a local clock; an unparseable row timestamp is **dropped rather than stored as fresh**, since storing it fresh would make this device defer forever to something that can never age out. Emission is a **dedicated interval on the chat component**, not the poll, because the web poll is event-driven while a beacon's whole meaning is a cadence. One S4a guarantee moved deliberately: the owed-verdict check now runs **ahead of** the rank check, because S4b made a demoted admin holding a durable staged sweep newly reachable and gating it behind the rank check would freeze that device permanently. |
| **The send lock, web** — **LANDED `000d9f8`** | `apps/web/src` | Unplanned, and forced by the staged sweep. Both send methods take `withGroupMlsLock` across seal **and** publish. See [§ What the client sprints found](#what-the-client-sprints-found), item 3. |
| **S5 + S6 — the iOS and Android mirrors** — **LANDED `1f230ab`** | `apps/ios/Lakko`, `apps/android/.../app` | The same shape on both: `CommitterSweep`, `CommitterBeacon`, `PendingRequests` beside the existing `CommitOutbox`, driven from `ChatRepository` / `ChatViewModel` / the members sheet, plus the send lock (Android's `GroupSend.kt` `runGroupSend`). **Both natives run a periodic long poll, so unlike web they satisfy the worst-case-poll constraint the beacon's freshness window is derived against** — the takeover works there as designed. Platform shapes that differ **by necessity, not by taste**: Android brackets the whole adjudication in one provider region, because between two applied rows the group is applied-but-not-persisted and a sibling group's restore must never observe that, and its sweep outbox entries carry a marker, because a sweep entry is written while the group deliberately sits **below** its recorded epoch — which the legacy reconciler reads as a torn write and would discard. iOS moved the lock seam up a layer, since its only lock primitive lives on the repository. Android also took the group lock on `broadcastControlToGroup`, **reversing a documented decision** in the ADR-0042/0043 subsystem that had declined it: that made it the one Android send path that could straddle an epoch, and a straddled routing secret means a member silently never learns the rotation and defers forever. |
| **S7 — external-join convergence** — **LANDED `576ea14`** (core + both FFI surfaces + all three clients) | `crates/lakko-mls` (**invariant #3**), both FFI surfaces, `apps/{web,ios,android}` | **An approval is now an `Add` proposal the approving admin authors itself**, carrying the joiner's retained KeyPackage and swept like any other. The last direct-commit path in a role-table group is gone, and G-38 residual 1 closes for the external-join path. Item 6 of [§ What the client sprints found](#what-the-client-sprints-found) is resolved with it — `refuse_if_proposals_pending` no longer reaches the approve, because there is no direct commit left for it to guard. **Decision 4's own prescription was rejected on contact and the reasons are the useful part — see [§ Why decision 4's design does not survive](#why-decision-4s-design-does-not-survive).** Two consequences it did not anticipate: `pending_external_joins` is **NOT** retired (the quarantine is load-bearing), and the API **splits** rather than retiring the direct arm, because `PublisherGroup` carries no role table and the proposal path refuses a table-less group by construction. |
| **S8 — docs** — **PARTLY DONE (2026-08-20, extended 2026-08-24 and 2026-08-25)** | `docs/` | Flip this ADR's status; reconcile `known-gaps.md` G-38 residual 1 and `invariant-index.md`; CLAUDE.md ADR-table row + `lakko-mls` module-map row. Done 2026-08-20: this ADR reconciled to the built core, ADR-0022's three transfer-as-follow-up references, the UX spec's owner dead-end, the CLAUDE.md ADR row + `lakko-mls` module-map row, and **C-63** minted in `invariant-index.md` + `calls.md`. Done 2026-08-24: the S3b and S3c reconcile — this ADR's three stale claims corrected in place, [§ The beacon, as built](#the-beacon-as-built) + [§ Why not `lakko-presence`](#why-not-lakko-presence) + [§ The filter obligation](#the-filter-obligation-the-beacon-created-and-why-a-beacon-is-an-event-rather-than-a-drop) + the indexed-invariant argument added, and CLAUDE.md's `lakko-contacts` / `lakko-ffi-extension` / `lakko-mls` module-map rows, ADR-0045 table row and fuzz-target list brought up to date. **No new indexed invariant** — argued, not defaulted. Done 2026-08-25: the client reconcile — this ADR's ~ten "inert" / "the clients are not built" passages corrected in place rather than bulk-deleted (several carry reasoning that survives the correction), the S4a/S4b/S5/S6 rows above, [§ What the client sprints found](#what-the-client-sprints-found), the CLAUDE.md ADR-table row + the `lakko-contacts` / `lakko-mls` module-map rows, **C-64** minted in `invariant-index.md` + `calls.md` with C-61 and C-63 amended, `known-gaps.md` G-38 residual 1 re-scoped to the external-join path, and the UX spec reconciled against what was actually built. **Still owed: the status flip**, which is the maintainer's, and which this pass deliberately did not take. |

**Four sprints landed that this table never had a row for. The first two were
forced by the built core rather than chosen; the last two are the FFI back-fill and
the decision-2 beacon.**

| Sprint | Surface | What changed |
|---|---|---|
| **S2b — the staged sweep** — **LANDED `fe3de6c`** | `crates/lakko-mls` (**invariant #3**) | `stage_pending_proposals` / `has_staged_sweep` / `merge_staged_sweep` / `abandon_staged_sweep`; `commit_pending_proposals` becomes the stage-then-merge pair. The rollback decision 2 layer 4 requires, built as the **absence of the eager merge** rather than as a retained snapshot — see [§ The staged sweep, as built](#the-staged-sweep-as-built). **New client obligation:** reach a verdict; never leave a sweep staged. |
| **S2c — ownership transfer + `owner_survives`** — **LANDED `e780952`** | `crates/lakko-mls` (**invariant #3**), and it mints **C-63** | ADR-0022's deferred follow-up, plus the commit-level never-orphan-the-owner rule that closes a pre-existing hole in `set_group_roles`. See [§ Never leave the group without an owner in it](#never-leave-the-group-without-an-owner-in-it). |
| **S3b — the FFI back-fill** — **LANDED `6c77cd6`** | `crates/lakko-ffi-uniffi` + `crates/lakko-ffi-wasm` | The staged pair and ownership transfer on both surfaces, in lock-step: `stagePendingProposals` / `hasStagedSweep` / `mergeStagedSweep` / `abandonStagedSweep`, `transferOwnership` / `proposeOwnershipTransfer` / `OutgoingOwner`. The S4 prerequisite this section previously recorded as missing. |
| **S3c — the decision-2 committer beacon** — **LANDED `94825ec` core, `94da492` FFI, `5d6af94` + `f53d11a` the client filters** | `crates/lakko-contacts` (a new envelope kind, no `lakko-mls` change — **invariant #3 untouched**), both FFI surfaces + `crates/lakko-ffi-extension`, and six client receive paths | Variant 1, the maintainer's chosen heartbeat: a field-less 27-byte beacon envelope, the pure `acting_committer_verdict` fold, the `should_emit_beacon` throttle, four constants, `Group::committerRank` for ranking a beacon's sender, a fuzz target, and the service-envelope filter arms that keep a beacon off the timeline and out of a notification. See [§ The beacon, as built](#the-beacon-as-built). **The emitter and the observation store were NOT part of it** — they landed in S4b / S5 / S6, and until they did the mechanism was inert. |

The mute fix (`create_application_message` parking the pending set) rode along in
`fe3de6c` and needed no sprint of its own: it touches one send path, and
**the clients inherited it with no work** — see
[§ Storing a proposal is not free: it muted the whole group](#storing-a-proposal-is-not-free-it-muted-the-whole-group).
The send path nevertheless needed a **client-side** fix of its own, for a different
reason the core cannot address: `000d9f8` above, and
[§ What the client sprints found](#what-the-client-sprints-found) item 3.

~~**S3b — the FFI back-fill — is a prerequisite for S4 and does not exist yet.**~~
**S3b — LANDED `6c77cd6`.** The claim that followed here — *"neither
`lakko-ffi-uniffi` nor `lakko-ffi-wasm` exposes `stage_pending_proposals`,
`has_staged_sweep`, `merge_staged_sweep`, `abandon_staged_sweep`,
`transfer_ownership`, `propose_ownership_transfer` or `OutgoingOwner`"* — was true
when written and is **no longer true**: all seven are on both surfaces, in
lock-step, and a client can now stage-and-adjudicate rather than being confined to
the eager-merge `commitPendingProposals` path that decision 2 layer 4 says a
non-owner committer must not use. The UX spec's owner dead-end is reachable from a
client. Kept rather than deleted because the prerequisite ordering it states is
still how S4 depends on S3b.

~~**The table does not yet scope decision 2's variant 1.**~~ **Variant 1 — LANDED
in the core and on both FFI surfaces** (`94825ec` / `94da492`), plus the six client
filter arms (`5d6af94` / `f53d11a`). This paragraph predicted the shape correctly —
*"an envelope kind in `lakko-contacts`, both FFI surfaces, and a beacon-plus-freshness
fold"* — and got one thing wrong, recorded because it is the reason the sprint cost
more than this predicted: the fold **is** in the core, but the beacon collides with
`parse_body`'s lossy-UTF-8 legacy fallback, so the sprint also had to add a
service-envelope filter to **six** client code paths (foreground and background, on
three platforms) before a beacon could ride the journal without appearing as a chat
bubble. See [§ The filter obligation the beacon
created](#the-filter-obligation-the-beacon-created-and-why-a-beacon-is-an-event-rather-than-a-drop).
It also predicted `lakko-presence` might serve; it does not, and
[§ Why not `lakko-presence`](#why-not-lakko-presence) says why. ~~**What remains
unbuilt is the part that makes any of it do something: the emitter and the
observation store.**~~ **Both landed in S4b / S5 / S6** (`02f0037`, `1f230ab`):
`composeCommitterBeacon` now has a production call site on every client
(`apps/web/src/routes/Chat.tsx`, `apps/ios/Lakko/Chat/ChatRepository.swift`,
`apps/android/.../chat/ChatRepository.kt`), each stores observations keyed on the
sender's signature key, and each drives `actingCommitterVerdict` at sweep time.
The warning this paragraph carried — *a reader who takes "the beacon landed" to
mean the election works has mis-read it* — has **spent its purpose and is retained
as history, not as a caution**; the election works, subject to the caveat that has
never been about the wiring: **a heartbeat removes the wait, not the race**, which
is why the stand-down and the staged rollback are built alongside it rather than
instead of it. **Variant 2 needed no new sprint** — the stagger would have been
client orchestration inside the same sprints — and it remains the documented
fallback if the beacon's journal-litter and activity-schedule cost proves worse in
practice than on paper.

**The outbox/lock layer is the least symmetric surface and ~~will cost~~ cost the
most per platform.** Web has `withGroupMlsLock` plus an atomic IndexedDB transaction
and no single create+persist+publish wrapper; iOS has `withCommitLock` (a
`@MainActor` task chain) plus an ordered, non-atomic two-step persist; Android has
the two-tier `groupLock` + `providerLock` with `prepareCrashSafeCommit` inside the
provider lock and the publish outside it. Any change to the commit sequence has to
be expressed three different ways — budget for that rather than assuming the web
reference translates. **This held, and the divergences it predicted are exactly the
ones the mirrors recorded** (the S5+S6 row above): Android's single-provider-region
adjudication and its sweep-entry marker, and iOS's lock seam moved up a layer. None
of the three is a stylistic choice; each falls out of that platform's existing
durability shape.

**Five more sprints landed after S7 that neither table has a row for, and a reader
of the tables alone would conclude the arc stopped at S7.** They are listed here as
a pointer rather than re-argued, because each is written up where it belongs:

| Landed | Surface | What changed |
|---|---|---|
| `c50bdd7` · `0274fa7` · `8763f3e` · `5b2b654` | `crates/lakko-mls`, all three clients | The seven `lakko-security` findings — see [§ What the security review found](#what-the-security-review-found). Minted **C-66**, **C-67**, **C-68**. |
| `0274fa7` | all three clients | **Ownership transfer, end to end** (UX §6.1). The core half landed in `e780952`; this is the affordance, and it closes the owner dead-end in the product as well as in the crate. |
| `8763f3e` | `crates/lakko-mls` + both FFI surfaces + all three clients | **`Group::is_active()` / `isActive`** and the self-eviction state (UX §6.2) — the case where your own removal lands. Tearing down the device's own push row here closes the half of **G-36 residual 2** ADR-0043 recorded as unclosable. |
| `8018e8b` | `crates/lakko-mls` (`refusal.rs`) + both FFI surfaces + all three clients | **Typed refusal reasons.** `RefusalReason` (18 variants) + `RefusalDisposition` + `Error::MembershipRefused`, so a client selects copy from a type rather than substring-matching core prose. |
| `21f6c8c` · `1839da7` | all three clients (+ one core fix) | **Automatic re-send of a request the queue had no room for**, triggered by an observed shrink of the pending set rather than by a timer. The core fix is the one that mattered: `approve_external_join` was re-wrapping its inner error in prose and destroying the `RefusalReason` it had just produced. |
| `46c7aa4` | all three clients + `docs/` | **S7's two client-visible residuals** — the `Not applied (n)` surface, and the honest wait copy plus user-driven repair on the joiner side — and **[ADR-0046](0046-external-crypto-review.md)**, which moved this ADR's external-review debt to **G-39**. |

---

## Alternatives rejected

- **Option A, a DS-side epoch gate.** Rejected on the freeze primitive (§ Option
  A), the partial coverage of DeviceGroup traffic, the posture change, and — most
  decisively — the fact that it does not remove the need for Option C's rollback
  and retry. Recorded in full rather than dismissed, because the recon question it
  turns on (can the DS read a commit's epoch pre-decrypt?) has a clear **yes**, and
  a future reader should not have to re-derive that the answer being *yes* is not
  what makes the option viable.
- ~~**Option B, proposal-based commits.** Rejected **for now** on
  proportionality.~~ **This rejection is withdrawn. Option B is the chosen
  option** — see [§ Decision](#decision). The struck reasoning rested on a
  proportionality judgement that is a frequency claim the recon had already, and
  correctly, declined to make; [§ Why the choice moved to Option
  B](#why-the-choice-moved-to-option-b) supplies the scale argument that answers
  it.
- **Option C, client-side loser rollback and bounded retry.** Rejected **as a
  standalone option**: it repairs a fault whose frequency scales with (admins ×
  churn) using a mechanism whose convergence degrades in the same variable, and
  its retry needs a per-intent "already satisfied?" predicate that the recon
  identified no need for and therefore did not cost. **Not discarded** — its
  retained-epoch-N-snapshot rollback survives as a required component of Option B,
  scoped to the acting committer on the non-owner path (decision 2), where it
  needs neither the intent replay nor the idempotency predicate.
- **Option C′ as a separate shipped companion.** Withdrawn as a standalone: under
  B the journal-head re-read is **promoted** from an optional narrowing to a
  required step of the committer's stand-down rule (decision 2, layer 3). The
  recon's warning still stands and now applies to B's stagger: a window-narrowing
  measure must never be described as a fix.
- **Option D, do nothing.** Rejected on the silent-plus-permanent-plus-no-
  diagnostic shape, independent of frequency.
- **A compatibility path / dual commit mode for Option B.** Rejected outright —
  there is no deployment and no user to be compatible with, and a legacy
  direct-commit fallback would be a second ungated route into group membership,
  which is the hazard rebuilt under a prudent-sounding name. See
  [§ No migration, and no compatibility path](#no-migration-and-no-compatibility-path).
- **A committer heartbeat / liveness ping on the journal, as Option B's absence
  signal.** Rejected: it materialises a periodic per-group activity artifact on the
  zero-trust DS for every group at all times (pressure on invariant #5 for a signal
  the DS has no analogue of today), and it still needs a timeout to interpret its
  own absence — it changes what you wait for, not that you wait. B infers
  **inaction** from the journal instead, which is both cheaper and closer to what
  the user actually cares about.
- **Authenticating DS publishes as a prerequisite for Option A.** Rejected as an
  enabler here: the natural authenticator is group membership, which is exactly
  what the zero-trust DS must not learn (invariant #5). Any membership-blind
  authenticator (PoW, a bearer token) raises the cost of the freeze without
  bounding it — the freeze is permanent and one shot suffices.
- **An advisory "membership operation in progress" lease posted to the journal.**
  Rejected: claiming the lease is itself a racy journal append with no arbiter, so
  it reduces the window without closing it — strictly worse than Option C′, which
  achieves the same narrowing with no wire artifact.
- **Making the loser's failure loud without repairing it** (surface the fork as an
  error and ask the user to rejoin). Rejected as a half-measure that is worth
  building *only* as a fallback inside Option C, where it is exactly what happens
  when the retry budget is exhausted.
- ~~**Deferring the eager merge in the core (ADR-0040's Option B).** Not an
  alternative here — it would make Option C cleaner (`clear_pending_commit`,
  `openmls-0.8.1/src/group/mls_group/mod.rs:374`, would discard the loser's staged
  commit with no snapshot restore at all) but it is an invariant-#3 change that
  ADR-0040 already weighed and deferred. Noted as the refinement that would retire
  Option C's retained-snapshot machinery if a future arc reworks the commit FFI
  anyway.~~ **This is no longer a deferral: it is what was built** (`fe3de6c`,
  sprint S2b). It is exactly the mechanism this bullet describes —
  `clear_pending_commit` discarding a staged commit with no snapshot restore at
  all — **scoped to the sweep path only**: the four direct-commit paths keep
  merging eagerly and behave byte-for-byte as they did, so the commit FFI did not
  have to be reworked and ADR-0040's outbox wraps both shapes unchanged. The
  invariant-#3 weight ADR-0040 was avoiding is real and was paid here, under this
  ADR. See [§ The staged sweep, as built](#the-staged-sweep-as-built).

---

## Consequences

> **This section was written for Option C and is now rewritten for Option B, the
> option that was chosen and built.** The Option-C list is kept below it, struck
> and labelled, because it is the honest record of what the recon expected to pay
> — and because two of its items (the add-path KeyPackage and the approve-path
> stale proposal ref) are *still* open questions, just for a different reason.

**Positive (Option B, as built).**

- The race is removed in the steady state rather than repaired. Concurrent
  membership intents become concurrent **proposals**, which compose; the loser of
  a race loses a proposal, which costs nothing, instead of a commit, which forks
  it off the group silently and permanently.
- **No server change, no wire-format change, no schema change and no new attack
  surface.** The whole mechanism is RFC 9420 proposals and commits that the DS
  already relays opaquely, plus the ADR-0022 role table that already exists in the
  authenticated GroupContext. There is no new artifact on the journal and no new
  per-group server state — which is exactly what Option A would have cost.
- **The committer election needs no protocol.** `committer_rank` is a pure total
  function of authenticated shared state, so there is no election message, no
  handover, and no committer state to get out of sync
  (`every_member_independently_derives_the_same_committer`).
- **The rollback is per-group**, because it is `clear_pending_commit` rather than
  a whole-provider restore. That is strictly safer than the abort path already
  shipped under ADR-0040, which rolls **every** group back
  (`abandoning_a_sweep_leaves_a_sibling_group_untouched`).
- **`owner_survives` closes a pre-existing hole.** An owner could always
  hand-build a role table naming a key no leaf holds and permanently brick the
  group; every peer now refuses that commit, whether or not anyone uses the
  transfer API. Indexed as **C-63**.
- **Two features fall out that the product did not have:** a member can leave a
  group, and an owner can hand one on. ~~**Neither has reached every client.**~~
  **Both have, and the dead end is closed to a user as well as in the crate.**
  `Leave group` reached the two natives in `c50bdd7` and the ownership-transfer flow
  reached all three in `0274fa7`. Two shapes are worth keeping from how they landed:
  `Leave group` is **disabled rather than hidden** for the owner, carrying the
  precondition as its reason — hiding it would leave an owner scanning for the exit
  and finding nothing — and the transfer's **direct arm is structurally unreachable**
  on every client, because `isOwner` and `usesProposalPath` are the same read, so
  each drives `proposeOwnershipTransfer` alone.
- **The election is real, not a design.** All three clients emit a throttled beacon
  from a foregrounded chat screen, store the observations and fold them at sweep
  time, so an admin-only group is served by whichever admin is running the app
  rather than waiting on an absent owner. ~~**With one honest platform asymmetry:**
  the web client has no periodic poll at all~~ — **closed.** The freshness window is
  derived against a worst-case *poll* interval, and the web client's poll used to be
  event-driven only (mount, a user action, the Refresh button), so a takeover there
  could lag well past the window. The web chat route now runs an automatic receive
  loop: a DS long poll with a 25 s `wait` (`apps/web/src/ds/long-poll-loop.ts`),
  running while the chat is open and the tab visible, or during a call — the same
  foreground condition the web beacon emitter already required. All three clients
  now meet the constraint.

**Negative / new risks (Option B, as built).**

- **An invariant-#3 change to the audited MLS state machine**, and the largest one
  since ADR-0022 — a new authorisation rule (`authorise`), a new commit-level rule
  (`owner_survives`), a new commit lifecycle (stage / merge / abandon) and a park
  on the send path. `lakko-security` **has** run on all of it (2026-08-29,
  REQUEST CHANGES, seven findings, all closed — see
  [§ What the security review found](#what-the-security-review-found)); the
  **external crypto review is tracked in [G-39](../security/known-gaps.md)**
  per [ADR-0046](0046-external-crypto-review.md).
- **A new client obligation that can wedge a group: reach a verdict.** A staged
  sweep freezes that group's own membership operations, and the staged commit is
  **durable**, so a client that stages and never adjudicates leaves membership
  frozen across restarts. This was the sharpest thing the client sprints had to get
  right, and **all three discharge it the same way**: every sweep adjudicates what
  it staged, an owed verdict is resolved at restart through ADR-0040's existing
  `sweepPendingCommits` seam rather than a second restart path, and the owed-verdict
  check runs **ahead of** the acting-committer rank check — because a demoted admin
  holding a durable staged sweep would otherwise be frozen permanently, and
  re-publishing is safe (peers refuse a demoted author's commit, so it loses and is
  abandoned, which is the unwedge). An unreadable verdict deliberately **leaves the
  sweep staged**: merging on an unread verdict is the eager shape this arc exists to
  remove, and abandoning would discard a commit already on the wire.
- **A send is not blocked by a pending commit, which turns an epoch straddle from a
  race into a certainty for the duration of a verdict read.** Closed on all three by
  serializing seal-and-publish under the per-group MLS lock — but only the **local**
  half; see [§ What the client sprints found](#what-the-client-sprints-found) item 3
  for the bound and what closing the rest would cost.
- **Any client discipline seamed onto a direct-commit call site has to be re-seamed
  onto the sweep**, and C-61 is the worked example: a removal that becomes a
  proposal seals a rotated push-routing secret at an epoch the target can still
  open. Found and fixed during the client sprints, from both ends
  ([§ What the client sprints found](#what-the-client-sprints-found) item 4), and
  recorded here because it is a *class*, not an incident.
- **Concurrent role changes do not compose where concurrent adds do — and the failure
  is a wedge, not a loss.** RFC 9420 admits one `GroupContextExtensions` proposal per
  commit, and OpenMLS enforces it as a hard build failure (`TooManyGCEProposals`), so a
  pending set holding two cannot be swept **at all** — and since the store empties only
  on a merge and `refuse_if_proposals_pending` closes the direct-commit escapes, every
  committer that holds both has its membership operations wedged.
  `ProposalKind::RoleChange` carries no payload for the client to reconstruct from.
  ~~Web and iOS refuse the second and say so; **Android does not**.~~ **Closed in the
  core 2026-08-28** by a propose-time refusal plus a sweep-side bound of one
  (**C-65**); see [§ What the client sprints found](#what-the-client-sprints-found)
  item 5. The remaining follow-up is client copy, not enforcement
  ([§ Open questions](#open-questions)).
- **Membership can wait indefinitely** when no candidate is running the app — a
  visible, honest wait replacing a silent fork, but still a behaviour that did not
  exist before, and one ownership transfer narrows rather than removes.
- **A new authority shift:** an elected committer can selectively censor
  proposals, and partial censorship is bounded by nothing in this design.
- **The race is narrowed, not eliminated** — two candidates in a handoff can still
  both commit, which is why the stand-down re-read and the staged rollback are
  required steps rather than optional ones.
- ~~**The decision-2 liveness variant is still unchosen**, so the sweep trigger —
  the thing that makes a `waiting` request ever end — does not exist in the core
  at all and is entirely client orchestration that has not been built.~~
  **Corrected: the variant is chosen (variant 1, the heartbeat) and its core half
  is built** — the beacon envelope, the `acting_committer_verdict` fold and the
  `should_emit_beacon` throttle are in `lakko-contacts` and on both FFI surfaces
  ([§ The beacon, as built](#the-beacon-as-built)). ~~**The sweep trigger still does
  not fire** … `composeCommitterBeacon` has zero production call sites~~ —
  **corrected again, and this time the bullet closes.** The emitter, the observation
  store and the fold landed on all three clients (`02f0037`, `1f230ab`), so a
  `waiting` request ends when any candidate next has the group open. The platform
  asymmetry this bullet used to keep — web had no periodic poll, so a web-only
  group's takeover could lag past the freshness window — is closed too, by the web
  chat route's long-poll receive loop (see the bullet above).

**Neutral (Option B, as built).**

- The owner acting alone — the overwhelmingly common case in a small group — is
  rank 0, sweeps its own proposal immediately, and sees today's behaviour.
- A group with **no role table** keeps direct commits, unchanged
  (`a_group_with_no_role_table_has_no_proposal_path`), and `Group::create` has
  seeded one since ADR-0022, so no freshly created group reaches that path.
- The four direct-commit paths still exist and still merge eagerly, so every
  caller that has not migrated behaves byte-for-byte as before. ADR-0045 forbids
  keeping them permanently. ~~**Three of the four have no production caller left in a
  role-table group** … and the fourth, `approve_external_join`, goes with **S7**.~~
  **All four are now unreachable from a role-table group.** Add, remove and
  role-change propose on all three clients, and S7 (`576ea14`) made the fourth a
  proposal too. What survives is `pub(crate) approve_external_join_directly`, whose
  one caller is `PublisherGroup::approve_external_join` and which **refuses** a group
  carrying a role table — so the direct arm exists exactly where the proposal path
  cannot work (a `PublisherGroup` seeds no role table, hence has no committer) and
  nowhere else.
- Chat sends are unaffected by a pending proposal, and — for *decryptability* —
  unaffected by a staged sweep. **The second half of that sentence was true of the
  core and false of the clients**, and is what item 3 of
  [§ What the client sprints found](#what-the-client-sprints-found) is about: not
  being *blocked* by a staged sweep is exactly what lets a send straddle its epoch.

<details>
<summary><strong>The original Option-C consequences, kept for the record</strong></summary>

- ~~**Positive (Option C).** The silent permanent fork becomes an automatic bounded
  retry, and its worst case becomes a **visible** error rather than a mystery. No
  `lakko-mls` change (invariant #3 untouched), no FFI change, no wire change, no
  server change, no schema change, no new attack surface. The rollback reuses the
  whole-provider restore whose safety argument is already written down and already
  reviewed. The Welcome-ordering constraint closes a *worse* failure (a phantom
  three-party fork) that exists latently today.~~
- ~~**Negative / new risks (Option C).** A second durable snapshot per in-flight
  commit on every client (transient — dropped on the verdict). One extra journal
  fetch on the interactive membership path. Two unresolved retry mechanics (the
  add-path KeyPackage, the approve-path stale proposal ref) that the
  implementation must answer concretely, not paper over. A retry budget that
  bounds but does not eliminate livelock in a hot group. The race itself remains;
  the loser's dead commit remains in the journal for its TTL.~~
- ~~**Neutral.** The steady-state (no race) path is today's behaviour plus one
  retained blob, one extra fetch, and a deferred second publish. Peers observe no
  change at all.~~

**Two of those are still live**, for reasons that outlived Option C: the
Welcome-ordering constraint (publish the Welcome only after the verdict) applies
verbatim to the staged sweep, and the retry budget's removal is now recorded in
decision 2 rather than here.

</details>

### Invariants touched

> **Read this section as the recon's cross-option comparison.** It weighs A, B
> and C side by side, which is what it was for; the **Option B** clauses are the
> ones that describe what was built, and each is updated below with what the build
> settled.

- **#1 (no plaintext PII on the server) — unaffected** by every option. ~~The
  retained snapshot is client-local and device-key-wrapped exactly as the existing
  one is.~~ **As built there is no retained snapshot at all** (S2b staged the
  commit instead), so #1 is untouched with nothing to argue: no new client-side
  artifact of any kind, and nothing new reaches the server.
- **#2 (the server sees only ciphertext + public artefacts).** **Option C: holds,
  untouched** — the server's inputs and behaviour are byte-identical. **Option A:
  no confidentiality change** (epoch, `group_id`, and `content_type` are already
  cleartext in every row the DS stores, RFC 9420 framing) **but a real posture
  change** — the DS gains a protocol-semantic refusal power it does not have
  today, contradicting its own module documentation (*"does not parse them, does
  not interpret epoch / seq / sender"*, `services/ds/src/main.rs:16-24`) and
  adding a fifth capability to the threat model's compromised-server column.
  Naming that plainly is the point; whether it is acceptable is the maintainer's
  call, and this ADR recommends against it for the availability reason, not the
  confidentiality one.
- **#3 (the MLS state machine is sacrosanct).** **Options A and C: untouched** —
  both are orchestration over the audited create-commit / process-incoming /
  snapshot primitives. **Option B: a direct change**, requiring an ADR, property
  tests, and external crypto review — specifically because it must add a
  proposal-sender authorisation gate where `ProcessedMessageContent::ProposalMessage`
  is today discarded unexamined (`crates/lakko-mls/src/group.rs:627`).

  **As built, the #3 surface is wider than that sentence anticipated** — four
  distinct changes, not one: the per-proposal authorisation gate (`authorise`,
  `9a60ab3`); the commit-level `owner_survives` rule (`e780952`); a **new commit
  lifecycle**, stage / merge / abandon, which defers the eager merge on the sweep
  path (`fe3de6c`, and the thing ADR-0040 deliberately declined to do); and a park
  of the pending proposal set around the application-message encrypt (`fe3de6c`).
  Property tests exist for all four (180 across seven binaries at the time this was
  written; 268 `#[test]` functions as of 2026-09-02 — 172 in the lib and 96 across
  seven integration binaries). ~~**The
  external crypto review is still owed, and `lakko-security` has not run on any of
  it**~~ — **`lakko-security` ran on 2026-08-29** (REQUEST CHANGES, seven findings,
  all closed); the **external crypto review has not**, and is tracked once in
  [G-39](../security/known-gaps.md) per
  [ADR-0046](0046-external-crypto-review.md) as a **release gate**. It is still the
  largest single open item behind this ADR.
- **#4 (no fallback to non-E2EE / fail-closed).** **Strengthened by Option C**: a
  device never remains silently on an epoch the group did not adopt, and a Welcome
  is never published for a group state that no member holds. There is no
  confidentiality break in the current bug — it is an integrity/availability
  divergence — so no E2EE guarantee is relaxed by any option. **Option A's
  rejection is fail-closed by construction** (refuse the publish), but its *failure
  mode* is the permanent freeze, which is availability, not safety.

  **Option B as built is fail-closed at every new decision point**: an
  unauthorised or unclassifiable proposal is dropped rather than stored
  (`ProposalKind::Unsupported` and an unresolvable sender are both refusals); the
  sweep filter excludes rather than includes on doubt; a commit carrying an
  unauthorised proposal is **refused, not merged**; a second stage over an
  un-adjudicated one is refused rather than silently replacing it;
  `abandon_staged_sweep` re-reads the state and refuses to report success on a
  group it did not actually roll back; and a failed unpark on the send path beats
  a successful send. The one deliberate **non**-refusal is a group with no role
  table, which keeps direct commits — the legacy/open case ADR-0022 already
  defined, and one `Group::create` cannot produce.

  **The clients are fail-closed at their own new decision points too**, and each
  refusal costs the *cheap* thing: an unreadable acting-committer verdict defers
  (a sweep attempt) rather than acting on a view of who else is running that this
  device could not read; the stand-down stands down on any unapplied row (one
  retry) rather than staging on a possible commit (a commit every peer drops); an
  unreadable journal verdict leaves the sweep staged rather than merging on an
  unread verdict or discarding a commit already on the wire; a beacon is not emitted
  when this device cannot establish that it is a candidate; and a sealed-but-unsent
  message never leaves the device if the seal's durable persist failed.
- **#5 (the social graph is not materialised on the server).** **Option C:
  untouched.** **Option A: a small erosion** — a per-group `current_epoch` row is
  new per-group server state (a commit-rate signal per group) where today the DS
  holds only opaque rows.
- **Invariant-index touchpoints — this claim is now withdrawn.** ~~This ADR
  proposes **no** new indexed `I-` / `C-` / `T-` entry.~~ It mints **two**.
  **C-63**: *a commit must never leave the group with an owner who is not a member
  of it* (`docs/security/calls.md` + `docs/security/invariant-index.md`). It earns
  an index row on merit rather than as bookkeeping for a new feature: the hole it
  closes has been reachable through `set_group_roles` since ADR-0022, is permanent
  when reached, and is refused on **every peer** independently of whether anyone
  calls `transfer_ownership`. **What the client sprints changed about C-63 is that
  two of its three enforcement layers acquired production callers**: layer (b), the
  sweep-side exclusion, is live on all three clients now that a sweep exists at all,
  and layer (c), the receipt boundary, was already live through
  `processIncomingVerifyingRoles`. ~~Layer (a), the authoring pre-check, still has
  **no production caller anywhere** — no client builds an ownership-transfer
  affordance~~ — **and since `0274fa7` it has one on every client**, because the
  transfer flow shipped and each client drives `proposeOwnershipTransfer`, which runs
  `plan_ownership_transfer` before any OpenMLS call. **All three layers now have
  production callers.** The reason (a) never being called was *a UX gap and not a
  weakening* is unchanged and still worth keeping: (c) is the boundary precisely
  because it does not trust (a).

  **C-64**: *the committer election and the staged sweep decide from authenticated
  state, and no epoch advances until the journal has ordered it* (Quick stats
  63 -> 64). ~~The decision-2 beacon (S3c) mints nothing~~ — that was argued rather
  than defaulted and it was **argued against a condition that has since lapsed**;
  see
  [§ Does the beacon earn an indexed invariant?](#does-the-beacon-earn-an-indexed-invariant-yes--and-it-is-minted-as-c-64).
  Short form: the deferral rested on the mechanism being **inert** and the index
  describing live properties, with the trigger named as *the first client that emits
  a beacon and folds a verdict*; S4b is that client and S5/S6 are the other two, and
  the mint is scoped to the election plus the sweep exactly as that section
  predicted a later mint would have to be. **The beacon's own invariant footprint,
  unchanged by the clients:** **#1** and **#5** untouched (the payload is field-less
  — no identity, no timestamp, no server-visible artifact of any kind); **#2** holds
  — the beacon rides an MLS application message, so the DS sees ciphertext and
  parses nothing, exactly as it does for the ADR-0042 routing envelope; **#3
  untouched by the beacon** — it lives in `lakko-contacts`, and
  `Group::committerRank` reads the existing `0xFF02` role table through an accessor
  rather than changing the state machine; **#4** — the codec is bounded and
  fail-closed (domain, then exact length, then version), and every client filter arm
  falls **through** to the chat parser on an unrecognised version rather than
  swallowing the row.

  Both entries also bear on **G-38 residual 1**, which stays a free-form known-gaps
  entry, matching how G-35 / G-36 / G-37 were kept. ~~It is now **closed for the chat
  membership paths and open for the external-join one**~~ — **it is closed for every
  membership path** since S7 (`576ea14`) converged the external-join approve onto the
  same sweep; see the S7 row in
  [§ Blast radius, per sprint](#blast-radius-per-sprint).

**Flag through `lakko-security` before merging any option.** Option C touches the
MLS commit publish ordering and the ADR-0040 durability argument. Option A touches
the zero-trust server's behaviour and adds an availability attack surface. Option B
touches the audited MLS state machine and relocates ADR-0022's authorisation
boundary. None of the three is a "no review needed" change. ~~**This is outstanding:
Option B's core is built and has had no `lakko-security` pass and no external
crypto review.**~~ **The `lakko-security` pass ran on 2026-08-29** against
`9a60ab3 → 8763f3e` and returned **REQUEST CHANGES**; its seven findings are closed
(see [§ What the security review found](#what-the-security-review-found)). The
**external crypto review has not run** and is tracked once, in
[G-39](../security/known-gaps.md), per
[ADR-0046](0046-external-crypto-review.md) — as a **release gate**, not a per-ADR
blocker.

---

## Open questions

- **~~The beacon has no emitter~~ — ANSWERED; the three things it named are built
  on all three clients.** The throttled emit from a **foregrounded** client (the
  C-59 obligation the core cannot check), the per-`(group, signature key)`
  observation store fed by the DS row's `created_at`, and the fold evaluated at
  verdict time with ranks re-derived through `Group::committerRank` against the
  current role table — `02f0037` on web, `1f230ab` on both natives, and all three
  are pinned as **C-64**. ~~**What is left of this item is a platform gap**~~ —
  **closed.** The beacon's freshness window is `2 × interval + worst-case poll`, and
  the web client used to have **no periodic poll at all** (mount, a user action and
  the Refresh button drove it), so on web a beacon could be observed well after the
  window it was sized for. Of the two fixes this item named — give the web chat a
  periodic poll, or derive its window from what it actually does — the first was
  built: the chat route long-polls its journal with a 25 s `wait`
  (`apps/web/src/ds/long-poll-loop.ts`, below the 30 s worst-case poll the window is
  sized against) while the chat is open and the tab is visible, or during a call.
  That is the beacon emitter's own foreground condition, so a web candidate that is
  beaconing is also observing, and the mechanism meets its design on all three
  platforms.
- ~~**`approve_external_join` is the last live direct-commit path, and it is now
  user-visible.**~~ **ANSWERED — S7 landed (`576ea14`).** The question this bullet
  said needed deciding — *what `Approve` means once it stops meaning "added"* — was
  answered by the build, and **not** the way decision 4 prescribed. It means *the
  approving admin authors the `Add` proposal itself*, so the approval reaches the
  wire as an ordinary MLS-authenticated artifact judged by the one existing rule. The
  three reasons decision 4's own design (store the stranger's proposal and mark it
  approved) does not survive contact are in
  [§ Why decision 4's design does not survive](#why-decision-4s-design-does-not-survive),
  and the last of them is why this bullet's *"everything S7 needs exists"* was
  optimistic: putting a stranger's proposal in the member store merges the **quotas**
  and hands a `/g/` link-holder the membership-freeze primitive C-66 had just closed.
  Two consequences, both stated in the S7 row: the `refuse_if_proposals_pending` dead
  end is gone because there is no direct commit left for it to guard, and G-38
  residual 1 closes for this path.

  **Two residuals S7 opened, both recorded rather than closed.** A sweep can drop an
  authorised approval and tell the **joiner** nothing — the group journal is
  unauthenticated and its DS pseudonym *is* the `group_id`, so a "your request was not
  carried" envelope would be a forgeable denial primitive any link-holder could post,
  and an epoch advance without a rendezvous is byte-identical to *nobody has approved
  yet*. The **approver** does learn (the `Not applied (n)` surface, `46c7aa4`); the
  joiner gets honest wait copy and a user-driven `Request again`. And nothing bounds
  how long a request sits in the quarantine when no committer is running — decision 2
  gives it no deadline by design, and the quarantine's depth is not a bound a client
  can state, since both caps are private to the core and absent from both FFI
  surfaces.
- ~~**Android does not refuse a second concurrent role change; web and iOS do.**~~
  **Closed 2026-08-28 in the core, and the call this bullet floated — "the natural fix
  is arguably the core rather than the third client" — is the one that was taken.**
  `Group::refuse_if_role_change_pending` refuses at propose time for every caller, and
  `sweep_keeps` bounds a sweep to one role change, which the propose-side guard does not
  cover because a peer can hold two **legitimately** (the ADR-0040 crash window). The
  bullet was also **wrong about the consequence**, and in the reassuring direction: the
  failure is not a silent loss but a **membership wedge** — OpenMLS refuses the whole
  sweep with `TooManyGCEProposals` — see
  [§ What the client sprints found](#what-the-client-sprints-found) item 5. Pinned as
  **C-65**.
- ~~**All three clients should surface the new core refusal as human copy, not the core
  diagnostic.**~~ **ANSWERED in `8018e8b`, and the answer was larger than the ask.**
  The bullet framed this as Android catching up: *Android has no client-side pre-check
  at all, so today it would show the raw `Error::Group` text*. Android now has the
  same narrow `hasPendingRoleChange` (`apps/android/.../chat/OwnershipTransfer.kt`) —
  and the drift turned out to be **three-way**, not one-way: web surfaced the spec's
  string, iOS threw its own near-variant prose, Android surfaced the diagnostic raw.
  Mapping core prose to fixed copy by substring would have rebuilt exactly that class,
  so the core gained a typed `RefusalReason` beside the sentence
  (`crates/lakko-mls/src/refusal.rs`) and each client selects its copy from the type.
  **What checking this entry found is still true and worth keeping:** web's
  `hasPendingRoleChange` (`apps/web/src/contacts/client.ts`) and iOS's
  (`apps/ios/Lakko/Chat/ChatRepository.swift`) are **correctly narrow** — each filters
  the pending set on the role-change kind specifically, so neither is stricter than
  the core rule and neither refuses a legitimate toggle merely because an unrelated
  add or remove is pending. **And one residual genuinely survives the copy landing:**
  both clients' *other* role-change call sites still carry doc comments describing the
  superseded keeps-one-discards-the-other mechanism
  (`apps/web/src/contacts/client.ts` and `apps/ios/Lakko/Chat/ChatRepository.swift`),
  which OpenMLS refuses outright as `TooManyGCEProposals`.
- ~~**Android surfaces a received proposal as a timeline row, and never stamps an
  observation for one.**~~ **CLOSED in `576ea14`, both halves.** UX §10 treats a
  membership request as a panel signal, and Android's `ChatClient` used to map
  `ProcessedMessage.Proposal` to a `System` row in the message list instead. It now
  raises its own `IncomingChatEvent.MembershipProposal`, and `SystemKind` carries no
  `Proposal` entry at all — deliberately, since `System` is the only variant
  `consumeEvents` turns into a `ChatMessage`, so an enum that *cannot name* a proposal
  is the strongest available statement of the rule. The deeper half is closed with it:
  `recordReceivedProposalObservation` gives a request arriving from **someone else** a
  DS timestamp, so §4's quiet window — which exists precisely so a request that
  resolves in milliseconds never flashes an annotation — now applies to every request,
  not only the reader's own. It refuses rather than guesses in two cases, and both are
  the point: an unstored proposal (the receipt gate refused it, so no sweep will carry
  it) and an unparseable DS timestamp, where falling back to the local clock would
  silently reintroduce the skew the DS timestamp exists to remove.
- **Android carries two `looksLikeCommitterBeacon` functions**, one in
  `me.lakko.app.chat.ChatClient` (3-arg: bytes, length, decoder lambda) and one in
  `me.lakko.app.push.PushServiceEnvelopeFilter` (2-arg: bytes, length — the peek
  only). They agree today, and both are host-testable, so nothing is broken — but a
  wire revision has to move two places. The duplicate is **avoidable**: the push
  filter already imports `looksLikeCallSignaling` from `chat`, so the dependency
  arrow exists and its own module doc argues for exactly this reuse (*"the proto
  oneof tags must have exactly one definition; a second copy would drift silently"*)
  — the natural shape is **chat owning the peek and push reusing it**, which is what
  was done for call signaling and not for the beacon. Recorded as a **cleanup, not a
  design**: the split came from how the two sprints were scoped, not from a reason.
- **Android's routing-secret arm has no positive host test in the foreground
  classifier.** `ChatClient.toEvent` injects its beacon arm and its body parser as
  defaulted parameters so a plain JVM test can drive them, but the ADR-0042 /
  ADR-0044 routing arm calls `decodePushRoutingSecret` **directly**, inside a
  `catch (_: Throwable)`. On the host there is no `liblakko_ffi_uniffi.so`, so that
  call throws `UnsatisfiedLinkError`, the catch swallows it, and a genuine routing
  envelope classifies as **chat** — so a test asserting the correct classification
  cannot pass. `ChatServiceEnvelopeTest` therefore only asserts the *negative* (the
  beacon double rejects a routing envelope). The background
  `PushServiceEnvelopeFilter.isServiceEnvelope` does not have this problem: it takes
  `decodesAsRoutingSecret` as a parameter and is positively tested. Closing it means
  giving the foreground routing arm the same injected shape its beacon neighbour
  already has.
- **The iOS NSE routing pre-filter still hardcodes its length.**
  `NsePushDecrypt.swift` carries `private let pushRoutingEnvelopeLen = 70` while the
  beacon peek beside it takes its length as an injected parameter sourced from the
  slim FFI's `committerBeaconEnvelopeLen()` — the beacon sprint added the export
  precisely so a wire literal would not sit unpinned in Swift. The routing literal
  is not unpinned (`push_routing::golden_pins_the_swift_prefilter_shape` asserts
  70 and `0x6c` in Rust), so the drift fails CI rather than a notification; but the
  two peeks now use two different mechanisms for the same job, and the newer one is
  the better one.
- **~~Decision 2's variant choice — a candidate heartbeat, or a staggered
  takeover?~~ Answered — the maintainer chose the candidate heartbeat (variant 1).**
  Variant 1 makes "active admin devices elect the committer" literal and removes the
  last timing constant. The cost stands as recorded and was accepted with it: it
  multiplies an anti-pattern ADR-0024 already accepted grudgingly (continuous
  journal rows, retained to the 30-day prune) and publishes each candidate's
  activity schedule to the whole group. The maintainer's ground for accepting it is
  that online/offline visibility is ordinary messenger functionality, so the privacy
  objection does not outweigh making the model literal. **Variant 2 is kept as the
  documented fallback**, not deleted — it needs no new signal and remains the answer
  if the beacon's cost proves worse in practice than on paper.

  **The correctness caveat is unchanged by the choice and must not be read as
  softened by it: a heartbeat removes the *wait*, not the *race*.** Each candidate
  computes over its own view of who beaconed recently, views differ by poll timing,
  and a beacon is stale by up to one interval — so two candidates can still both
  conclude they are the lowest-ranked active one. The stand-down journal re-read and
  the bounded rollback remain **required** in both variants. Anyone reading variant 1
  as correct-by-construction has mis-read it.

  **What the choice costs in scope**, which the ADR must not leave implicit: variant
  2 needed no new sprint, because its stagger would have been client orchestration
  inside the client sprints. ~~Variant 1 is a sprint that does not yet exist in the
  decomposition~~ — **it is S3c in
  [§ Blast radius, per sprint](#blast-radius-per-sprint), and it has LANDED end to
  end**: the core and both FFI surfaces in `94825ec` / `94da492` with the client
  filter arms in `5d6af94` / `f53d11a`, and the emitter, the observation store and
  the fold in `02f0037` (web) and `1f230ab` (both natives). It needed the predicted
  beacon envelope kind in `lakko-contacts` and **both** FFI surfaces;
  `lakko-presence` (exposed on wasm only, no UniFFI counterpart, no caller under
  `apps/web/src`) was evaluated and **not** used, for reasons sharper than its
  unwiredness — see
  [§ Why not `lakko-presence`](#why-not-lakko-presence). It also needed one thing
  this paragraph did not predict: a service-envelope filter arm on **six** client
  receive paths, because a beacon otherwise renders as a chat bubble through
  `parse_body`'s lossy-UTF-8 legacy fallback. ~~**The beacon-plus-freshness fold is
  in the core, but nothing drives it and nothing emits a beacon**, so S4 still cannot
  elect a committer~~ — **it does now, on all three clients.** The scope cost the
  choice actually carried is one core sprint, one FFI sprint, six filter arms and a
  per-client emitter-plus-store; and one thing nobody priced, recorded because it is
  the least obvious of them: **an emitter is a timer, and a timer on the send path
  widens every hazard that path already had** — which is how `000d9f8` came to be
  written.
- **~~The retry budget and its user-visible exhaustion state.~~ Answered — there is
  no retry budget.** The revised decision 2 removes both the cap and the terminal
  failure it produced: a request waits indefinitely rather than exhausting a budget,
  and the committer's own rollback needs no budget because it replays no intent
  (the proposals are the durable record of intent, and the winning commit consumed
  them). Recorded here rather than deleted because the UX spec was written against
  the budget and had to be reconciled to its removal.
> **The next four questions were framed against Option C's intent-replay retry,
> which is not what was built. They are re-framed here rather than deleted,
> because three of the four survive the change of option — a staged sweep still
> gets abandoned, and the questions are about what an abandoned commit leaves
> behind.**

- **The add-path retry and KeyPackage consumption.** ~~`add_member` consumes one of
  the joiner's published KeyPackages. Does a discarded commit release it, must the
  retry re-fetch from discovery, and what happens when the joiner has none left?~~
  **Re-framed, and smaller than it was.** Decision 2 already argued the fetch
  happens once at *propose* time under B, so a lost race costs no KeyPackage. The
  question that remains is narrower and concerns the built rollback: does
  `abandon_staged_sweep` leave the joiner's `KeyPackage` usable for the winner's
  sweep? The proposal survives abandonment by construction (the store empties on
  **merge**), so the answer should be yes — but it is asserted, not tested.
  ~~**Answer it before S4.**~~ **S4 shipped without it being answered**, which is
  where it now sits: still asserted, still untested, and now load-bearing on three
  clients rather than none.
- **The approve-path retry and the stale proposal ref.** ~~ADR-0027's pending
  external joins are epoch-bound and in-memory. After the replay the retained
  proposal is stale~~; ~~**decision 4 dissolves the in-memory half** — after S7 an
  external-join proposal is a stored MLS proposal like any other.~~ **S7 landed and
  dissolved neither half, deliberately.** The Lakko-side `pending_external_joins`
  `Vec` is **kept** — it is the quarantine that stops a `/g/` link-holder filling the
  member proposal store — so it is still outside the staged rollback (and, since
  2026-09-26, no longer in memory only: it is persisted as a row in the storage
  provider and read back by `Group::load`, see the ADR-0027 update note). One correction to the framing: it is **not epoch-bound**. The retained
  `KeyPackage` outlives epoch advances; only the joiner's *original* external proposal
  had to arrive at a matching epoch. What drops an entry is the joiner
  landing (`pending_external_joins()` filters members). **Two in-memory lifetimes now
  meet on the client side and the mismatch is handled rather than prevented:** the
  quarantine lives on the `Group` while the retry record lives beside the chat, and
  both the ADR-0040 crash-abort reset and the web rehydrate rebuild the `Group`
  mid-session — so a retry record can outlive its request with no restart, and each
  client's verdict re-reads the live retained set and **drops** rather than firing an
  approve the core would refuse. Still a UX question as much as a mechanism one —
  route through `ux-ui`.
- **Whether ~~Option C~~ the staged sweep generalises to the DeviceGroup and the
  channel PublisherGroup.** Unchanged and still open, and the answer today is
  **no**: `PublisherGroup::rotate` still calls `self_update` and merges eagerly,
  and DeviceGroup commits are untouched. The DeviceGroup's commits go to the same
  journal wrapped in the ADR-0015 envelope, and ADR-0028 OQ3's co-publisher approve
  is a `PublisherGroup` commit. ADR-0040 explicitly scoped itself to the four chat
  paths and noted the same parity question; this ADR inherits it, and
  [§ What Option B does not fix](#what-option-b-does-not-fix) item 5 restates it.
- **~~Whether Option C′ ships regardless.~~ Answered by the choice of B, and now
  built** — the journal-head re-read is a required step of the committer's
  stand-down rule, and it exists on all three clients immediately before staging.
  ~~It is client orchestration and does not exist yet~~; it is client orchestration
  and it does. Nothing in the core performs it, by design: the core cannot see the
  journal.
- **Whether `epoch_of_message` should gain a `content_type` sibling on the FFI.**
  ~~Option C's verdict step needs "is this row a Commit framed at N"~~ — **and so
  does B's**, because the staged sweep's verdict is the same question asked of the
  same journal. Today only the epoch is exposed
  (`crates/lakko-ffi-uniffi/src/lib.rs`, `crates/lakko-ffi-wasm/src/mls.rs`).
  ~~Epoch alone is a *sufficient* signal when combined with "this row is not one of
  our own artifacts"~~ — **that answer was wrong and wrong in the dangerous
  direction; see
  [§ What the client sprints found](#what-the-client-sprints-found) item 1.** The
  clients sidestepped it entirely by asking a question that needs no
  discrimination — apply the rows below our own commit, then ask whether the epoch
  moved — so **the addition is no longer on any critical path**. It survives as a
  smaller, genuinely optional question: a `contentTypeOfMessage` peek would let the
  **stand-down** discriminate instead of standing down conservatively on any
  unapplied row (item 2). An optimisation, decided explicitly rather than by
  omission.
- **The external crypto review.** `lakko-security` has run (2026-08-29) and its
  seven findings are closed; the external review has not, and it is now tracked
  **once**, in [G-39](../security/known-gaps.md), per
  [ADR-0046](0046-external-crypto-review.md) — which also states why it stopped
  being a per-ADR blocker: carried inside each MLS-touching document it made every
  one of them permanently un-Acceptable however finished it was, while specifying
  nothing an auditor could act on. It remains a **release gate**: no build reaches
  users while it is open. The surface has grown rather than shrunk — six sprints of
  MLS state-machine change **plus**
  three client implementations of the authorisation, ordering and rollback
  disciplines that change depends on. **The maintainer's call when to run them** —
  the ADR's own earlier framing offered "before S4, or once the clients are wired",
  and the clients are wired. Two specific things a review should be pointed at,
  because the client sprints created them: the **send lock** (`000d9f8` and its two
  native mirrors), which is an MLS-ordering discipline expressed three different
  ways, and the **C-61 re-seam** onto the sweep, which is a security ordering rule
  that moved call sites (Android's commit message asks for exactly this look). It is
  not a question that closes by itself.
- **The stale ADR index.** `docs/adr/README.md`'s `## Index` stops at ADR-0015, so
  it is missing thirty-one entries. (This bullet also said the `## Index` section was
  **duplicated**; it is not, and was not when the bullet was written — the file has one
  `## Index` heading. Corrected rather than deleted, because the miscount is the kind
  of detail a reader would otherwise chase.) Out of scope here; flagged for a separate
  docs pass.

---

## Sprint decomposition — **for Option C, which was NOT picked**

> **This is recon record, not a plan. Do not build from it.** The plan is
> [§ Blast radius, per sprint](#blast-radius-per-sprint), whose S0-S3 plus S2b and
> S2c are already built. This list is kept because it is the shape Option C would
> have taken, and because the contrast is the clearest statement of what choosing
> B avoided. Two of its steps name things that **no longer exist**: the
> retry-exhausted error state (there is no retry budget — decision 2 deleted it,
> and a client must not invent one) and the retained epoch-N snapshot (the built
> rollback stages the commit instead).

- **S0 — UX.** ~~The retry-exhausted error state~~ — **no such state exists**; the
  approve-path "request expired" state, and whether a membership commit shows a
  pending affordance at all. `ux-ui` first, per the repo's UX-first rule.
  (Superseded by the real S0, [`docs/ux/proposal-based-membership.md`](../ux/proposal-based-membership.md).)
- **S1 — resolve the two retry mechanics.** The add-path KeyPackage question and
  the approve-path proposal-ref question, answered in code before any client work.
  Possibly a small additive FFI peek (`contentTypeOfMessage`).
- **S2 — web reference.** ~~Retained epoch-N snapshot~~ inside the existing
  `saveSnapshotWithCommitEntry` transaction; the verdict step; the deferred second
  publish; the replay-and-retry with a bounded budget; the sweep extension. Vitest
  coverage that **discriminates** — a neutered verdict step must fail a named test,
  per the ADR-0044 discipline.
- **S3 — iOS mirror.** Same shape over `ChatRepository.swift` /
  `CommitOutbox.swift`, with the entry-first-then-snapshot reconciliation extended.
- **S4 — Android mirror.** The Kotlin twin, under the existing two-tier
  `providerLock` discipline.
- **S5 — Option C′** in all three (journal-head re-read before create).
- **S6 — docs reconcile.** Flip this ADR's status, resolve G-38 residual 1 in
  `known-gaps.md` with its remaining residuals stated honestly, add the CLAUDE.md
  ADR-table row.

---

## References

### What was built (2026-08-20)

Symbols rather than line numbers, per the anchor convention at the top.

- `crates/lakko-mls/src/group_proposals.rs` — `ProposalKind`, `ProposalRecord`,
  `Proposer`, `classify`, **`authorise`** (the one per-proposal rule, consumed at
  all three layers), `CommitEffects` / `commit_effects`, **`owner_survives`**
  (the commit-level rule indexed as **C-63**). Its module documentation carries
  the corrections to decision 3 that this revision folds back into the ADR body.
- `crates/lakko-mls/src/group_roles.rs` — **`committer_rank`** (decision 1),
  `RoleTable::with_owner_transferred`, `OutgoingOwner`.
- `crates/lakko-mls/src/group.rs` — `propose_add_member` /
  `propose_add_member_from_wire` / `propose_remove_member` / `propose_group_roles`;
  `transfer_ownership` / `propose_ownership_transfer`; `pending_proposals`,
  `own_committer_rank`, `commit_pending_proposals`; **`stage_pending_proposals` /
  `has_staged_sweep` / `merge_staged_sweep` / `abandon_staged_sweep`** (the
  rollback point); **`create_application_message`** (the park that keeps a pending
  proposal from muting the group); the proposal arm of
  `process_incoming_verifying_roles` (the receipt gate).
- Tests — `crates/lakko-mls/tests/group_proposals.rs` (22),
  `group_ownership_transfer.rs` (12), the untouched `group_admin_roles.rs` (7),
  and per-layer isolation tests as `#[cfg(test)]` lib tests in `group.rs`.
  **Verified firsthand by the maintainer on this tree:** `cargo test -p lakko-mls`
  180 passed across seven binaries, 0 failed; clippy on `lakko-mls` clean;
  `cargo check --workspace --all-targets` clean.

  > **These counts are a 2026-08-20 snapshot and every later sprint added to them.**
  > As of 2026-09-02 the integration binaries hold 37 / 12 / 8 `#[test]` functions
  > respectively — `group_admin_roles.rs` stopped being untouched at `a90fa90`, which
  > added the eighth — plus two integration binaries that did not exist on the tree
  > this snapshot names: `group_membership_state.rs` (5, the `is_active` accessor,
  > `8763f3e`) and `membership_refusal_reason.rs` (21, the typed refusals,
  > `8018e8b`). The snapshot is left standing rather than rewritten, because it is
  > what was verified on the tree it names.
- `openmls-0.8.1/src/group/mls_group/mod.rs` `is_operational` +
  `commit_builder.rs` `stage_commit` — the freeze and the durability of a staged
  commit, both of which are client obligations
  ([§ The staged sweep, as built](#the-staged-sweep-as-built)).
- `openmls-0.8.1/src/group/mls_group/application.rs:27-31` — `create_message`
  refuses on a non-empty proposal store: the mute.
- `openmls-0.8.1/src/group/public_group/staged_commit.rs`
  `ProposalQueue::from_committed_proposals` — by-reference resolution at commit
  receipt on **every** member, which is what makes "store at receipt" mandatory.
- `openmls-0.8.1/src/group/mls_group/proposal.rs:502`
  `MlsGroup::remove_pending_proposal` — public, contrary to this ADR's original
  decision-3 claim.

### What was built for decision 2 (2026-08-24)

The heartbeat, variant 1. Four commits; the core carries no `lakko-mls` change, so
**invariant #3 is untouched by this sprint**.

- `crates/lakko-contacts/src/committer_beacon.rs` (`94825ec`) — the 27-byte
  field-less envelope (`COMMITTER_BEACON_ENVELOPE_DOMAIN_V1` /
  `COMMITTER_BEACON_VERSION_V1` / `COMMITTER_BEACON_PAYLOAD_LEN`),
  `compose_committer_beacon` / `try_unwrap_committer_beacon` /
  `decode_committer_beacon` + `CommitterBeaconError`, the fold
  **`acting_committer_verdict`** over `BeaconObservation` →
  `ActingCommitter::{Act, Defer, NotACandidate}`, the throttle
  **`should_emit_beacon`**, and the three timing constants
  (`COMMITTER_BEACON_INTERVAL_MS`, `COMMITTER_BEACON_WORST_CASE_POLL_MS`,
  `COMMITTER_BEACON_FRESHNESS_WINDOW_MS`). 33 in-module tests. **Its module
  documentation is the authority** for the no-fields decision, the
  `lakko-presence` rejection and the constants' budget argument — this ADR's
  [§ The beacon, as built](#the-beacon-as-built) records them, it does not restate
  them in full.
- `crates/lakko-contacts/src/incoming_envelope.rs` (`94825ec`) — dispatch step 5,
  the `IncomingEnvelope::CommitterBeacon` **unit** variant and the
  `DispatchError::CommitterBeacon` arm.
- `crates/lakko-contacts/fuzz/fuzz_targets/committer_beacon_decode.rs` (`94825ec`)
  — the bounded decode **plus** the fold driven from the same attacker-controlled
  bytes, asserting two properties on every input: a `None` own-rank is always
  `NotACandidate`, and a `Defer` never names a rank at or above this device's own.
- `crates/lakko-ffi-uniffi/src/contacts.rs` + `crates/lakko-ffi-wasm/src/contacts.rs`
  (`94da492`) — `composeCommitterBeacon` / `decodeCommitterBeacon` /
  `actingCommitterVerdict` / `shouldEmitBeacon`, the `BeaconObservation` and
  `ActingCommitter` mirrors, all four constants, and `committerBeaconEnvelopeLen`.
- `crates/lakko-ffi-uniffi/src/lib.rs` + `crates/lakko-ffi-wasm/src/mls.rs`
  (`94da492`) — **`Group::committerRank(signatureKey)`**, the peer-facing twin of
  `ownCommitterRank` and the input `BeaconObservation.rank` needs.
- `crates/lakko-ffi-extension/src/committer_beacon.rs` (`94da492`, extended
  `f53d11a`) — `decode_committer_beacon` + `committer_beacon_envelope_len` for the
  iOS NSE; compose, the fold, the throttle and the timing constants are
  **deliberately absent**. 8 tests including
  `an_unfiltered_beacon_is_never_chat_text` (renamed 2026-09-29, when `parse_body` began classifying every `lakko/` service envelope as never-rendered) and
  `golden_pins_the_swift_prefilter_shape`.
- The client filter arms — background (`5d6af94`):
  `apps/web/public/sw-push-decrypt.js`,
  `apps/android/.../push/PushServiceEnvelopeFilter.kt` +
  `PushBackgroundDecrypt.kt`, `apps/ios/LakkoNotificationService/NsePushDecrypt.swift`
  + `NseDecryptDeps.swift`; foreground (`f53d11a`): `apps/web/src/chat/client.ts` +
  `routes/Chat.tsx`, `apps/android/.../chat/ChatClient.kt` + `ChatRepository.kt`,
  `apps/ios/Lakko/Chat/ChatClient.swift` + `ChatRepository.swift`. Plus the
  exhaustive-switch repair in both `MultiJoinerCall` dispatchers.
- ~~**No emitter.**~~ True when written; see the next section.

**Gates, verified firsthand by the maintainer on this tree** (this docs pass ran
`audit-index-check` and `spec-check` only, both clean): `cargo test -p lakko-contacts`
249 · `-p lakko-ffi-extension` 17 · `-p lakko-ffi-uniffi` 219 · `-p lakko-ffi-wasm`
139; web `tsc` clean with vitest 622; Android `:app:compileDebugKotlin` BUILD
SUCCESSFUL with 555 tests and 0 failures; iOS `xcodebuild` BUILD SUCCEEDED with
383/383 excluding the two dockerised-SFU cases.

### What the clients built (2026-08-25)

Four commits, no core change in any of them. Where the natives differ from the web
reference the difference is named in the S5+S6 row of
[§ Blast radius, per sprint](#blast-radius-per-sprint) and is forced by that
platform's durability shape, not chosen.

- `apps/web/src/chat/sweep.ts` + `pending-requests.ts` (`1de5fe7`),
  `committer-beacon.ts` (`02f0037`) — the web reference. The orchestration is
  expressed over injected ports so the whole of it runs in the plain `node` vitest
  environment: no IndexedDB, no wasm, no network. `apps/web/src/contacts/client.ts`
  supplies the adapters and is the sweep module's only production caller.
- `apps/ios/Lakko/Chat/{CommitterSweep,CommitterBeacon,PendingRequests}.swift` and
  `apps/android/.../chat/{CommitterSweep,CommitterBeacon,PendingRequests,GroupSend}.kt`
  (`1f230ab`) — the mirrors, same port-injected shape.
- The emitters, which are what ended the beacon's inertness:
  `apps/web/src/routes/Chat.tsx` (a dedicated `setInterval` on the chat component —
  **not** the poll, because the web poll is event-driven while a beacon's meaning is
  a cadence), `apps/ios/Lakko/UI/Chat/ChatViewModel.swift` (a `Timer` started and
  stopped with screen visibility, where `isForeground` requires **both** the screen
  visible **and** `UIApplication.shared.applicationState == .active`, since neither
  implies the other), and
  `apps/android/.../ui/chat/ChatScreen.kt` (a `repeatOnLifecycle(RESUMED)` loop —
  the lifecycle scope **is** the foreground gate, and the flag is still passed
  through explicitly so the rule is written down in one testable place).
- The send lock: `000d9f8` (web, both send methods deduplicated into one
  `sealAndPublish` under `withGroupMlsLock`) and `1f230ab`
  (`GroupSend.kt` `runGroupSend` on Android; the iOS twin on the repository).

**Two things about how it was verified are worth keeping**, because they are what
distinguishes a test that passes from a test that discriminates. Every claim of
discrimination in these commits was **measured by running a probe and recording its
failure**, then reverting the probe — hoisting the Welcome above the verdict, merging
instead of abandoning on a loss, putting the owed-verdict check back behind the rank
check, deleting the foreground gates, carrying a rank over from receipt, removing the
send lock (which reproduces both straddle directions), and re-keying either lock
globally (which deadlocks, so the per-group keying is load-bearing on all three). And
the oracles avoid simulating the property under test: the iOS straddle oracle drives
a **real second MLS member** replaying the recorded journal, so zero past-epoch
retention is whatever `lakko-mls` actually configures rather than what a double
asserts; Android's concurrency tests run on `Dispatchers.IO` with real gates, never a
confined dispatcher, because an earlier sprint found a confined dispatcher masking a
genuine defect.

**Gates, as recorded in the commits** (not re-run by this docs pass, which ran
`audit-index-check` and `reuse` only): web `tsc --noEmit` clean with vitest 706
across 46 files and `vite build` ok; iOS `xcodebuild` BUILD SUCCEEDED with 432 tests
and 0 failures excluding the two pre-existing dockerised-SFU cases; Android
`:app:compileDebugKotlin` BUILD SUCCESSFUL and `:app:testDebugUnitTest` 598 tests
with 0 failures and 0 errors, the two gates run separately.

### Prior art and the recon's sources

- **G-38 residual 1** (`docs/security/known-gaps.md`) — the gap this ADR
  addresses, and the source of the two candidate fixes it weighs.
- **[ADR-0040](0040-durable-pending-commit-outbox.md)** — the eager merge, the
  four commit paths and their core call sites, the durable outbox, the step-2
  atomic write, and its own deferred Option B (defer-merge in the core).
- **[ADR-0044](0044-push-routing-secret-recency.md)** — C-62 and the serializing
  per-`(account, group)` lock; the "a gate must run against current state" lesson
  and the discriminating-test discipline this ADR's S2 inherits.
- **[ADR-0022](0022-group-admin-roles.md)** — the role table and the
  peer-verified committer gate Option B would have to relocate to the proposal
  sender.
- **[ADR-0027](0027-external-join-approval.md)** — the epoch-bound in-memory
  pending external joins and the `WelcomeRendezvous`; the rejection of
  `commit_to_pending_proposals` for single-joiner targeting.
- **[ADR-0028](0028-group-join-ticket.md)** — `epoch_of_message` as the joiner's
  pre-decrypt epoch read, and the `/g/` capability that conveys the raw `group_id`
  to a non-member.
- **[ADR-0024](0024-typing-read-receipts.md)** — the chat path's existing ephemeral
  presence signals (typing + read receipts) that decision 2's variant 1 would
  extend, and its own Consequences entry naming journal-resident typing rows,
  retained to the 30-day prune, as an acknowledged anti-pattern *"mitigated by
  throttling + tiny payload"* — the cost a continuous heartbeat multiplies.
- `apps/web/src/chat/presence-ops.ts` — `foldTyping` / `activeTypers` /
  `pruneTyping` / `foldReadWatermark` / `isReadByEveryOther`, the working
  throttle-plus-expiry-window fold a candidate beacon would sit beside; driven by
  `apps/web/src/routes/Chat.tsx`, with iOS and Android twins.
- `crates/lakko-ffi-wasm/src/lib.rs:114` — `lakko-presence` re-exported
  (`PresenceToken`, `PresenceTracker`, `derive_token`, `current_epoch`): a live
  surface, **wasm only** (no UniFFI counterpart) and with no caller under
  `apps/web/src`, so it is exposed but unconsumed by the chat path.
- **[ADR-0032](0032-multi-joiner-call-links.md)** — `elect_committer`
  (`crates/lakko-calls/src/link_invite.rs:230`), the designated-committer
  precedent Option B would reuse, and its recorded weak-authentication residual.
- **[ADR-0015](0015-device-removal-authorization.md)** — the DeviceGroup journal
  envelope that makes DeviceGroup commits unparseable from outside, and hence
  invisible to any DS-side gate.
- **RFC 9420** (MLS) §6 (framing — `group_id` / `epoch` / `content_type` cleartext
  in `PrivateMessage`), §12 (commit and epoch-barrier semantics).
- `openmls-0.8.1/src/group/public_group/validation.rs:56-80` — ValSem003: a
  Commit is accepted only at the current epoch; an Application message is accepted
  at any epoch not in the future.
- `openmls-0.8.1/src/framing/message_in.rs:179-237` — `ProtocolMessage::epoch()`
  / `content_type()` / `is_handshake_message()`, all pre-decrypt.
- `openmls-0.8.1/src/framing/private_message_in.rs:39-46` — the cleartext
  `group_id` / `epoch` / `content_type` fields.
- `openmls-0.8.1/src/group/mls_group/proposal.rs:270,309,395` +
  `processing.rs:206,234` + `mod.rs:374,397` — the proposal / pending-commit API
  Option B would build on.
- `crates/lakko-mls/src/group.rs:1009` (`epoch_of_message`), `:582-632`
  (`process_incoming_verifying_roles`, and `:627` where a proposal is discarded
  unchecked).
- `services/ds/src/main.rs:16-29` — the DS's stated blindness and its absence of
  authentication; `services/ds/src/journal.rs:177` — the publish handler;
  `services/ds/migrations/0001_init.sql` — the two tables and the absence of any
  per-group state.
- `apps/web/src/ds/client.ts:45,171` — the DS pseudonym is the MLS `group_id`, hex.
- `apps/web/src/chat/client.ts:730-745` — the silent catch-and-continue that makes
  the fork invisible.
- `apps/web/src/contacts/client.ts:2126,2134,2381,2395` — commit, rendezvous,
  external-join proposal, and attribution, all on the one group journal.
- `apps/web/src/devices/sync/DeviceGroupSyncContext.tsx:464` (+ `remove.ts:165`,
  `attested-removal-policy.ts:49`, `orchestrator.ts:588`) — every DeviceGroup
  publish wrapped in the ADR-0015 journal envelope.
- `apps/android/app/src/main/kotlin/me/lakko/app/chat/ChatRepository.kt:215-238,2743,2786`
  — the `providerLock` whole-provider discipline and
  `resetGroupToDurableSnapshotOrThrow`, whose correctness argument assumes the
  durable snapshot is still at N.
