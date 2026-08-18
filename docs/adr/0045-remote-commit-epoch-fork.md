<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0045: Closing the remote-commit epoch fork — client-side loser rollback, weighed against a DS epoch gate and proposal-based commits (G-38 residual 1)

- **Status:** **Proposed.** No code. This ADR is the recon + the decision
  framing; the maintainer picks the option and implementation follows the pick.
- **Date:** 2026-08-18
- **Context:** Phase 3 — group-membership commits / MLS epoch integrity. Closes
  **G-38 residual 1**, the largest unfixed item on the client-concurrency list
  and the only one on it that **no client-side lock can address by
  construction**, because the second committer is a different device.
- **Relationship to prior ADRs.** **Extends
  [ADR-0040](0040-durable-pending-commit-outbox.md); supersedes nothing.** The
  eager merge inside the create-commit call, the durable pending-commit outbox,
  the fail-closed step-2 atomic write, and the idempotent restart sweep all carry
  through verbatim. ADR-0040 closed the **crash** window between publish and
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
most useful output of this recon, and it is what the recommendation rests on.

Option B is the exception: by making only one device commit, it removes the race
rather than repairing its loser, so it genuinely does not need the rollback.

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

### Option B — proposal-based commits

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

## Decision (recommended — the maintainer decides)

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

## Alternatives rejected

- **Option A, a DS-side epoch gate.** Rejected on the freeze primitive (§ Option
  A), the partial coverage of DeviceGroup traffic, the posture change, and — most
  decisively — the fact that it does not remove the need for Option C's rollback
  and retry. Recorded in full rather than dismissed, because the recon question it
  turns on (can the DS read a commit's epoch pre-decrypt?) has a clear **yes**, and
  a future reader should not have to re-derive that the answer being *yes* is not
  what makes the option viable.
- **Option B, proposal-based commits.** Rejected **for now** on proportionality:
  an invariant-#3 change plus a new proposal-sender authorisation gate plus a
  liveness single point plus a three-client UX change, against a hazard Option C
  repairs client-side. Explicitly **kept on file** as the correct-by-construction
  shape.
- **Option D, do nothing.** Rejected on the silent-plus-permanent-plus-no-
  diagnostic shape, independent of frequency.
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
- **Deferring the eager merge in the core (ADR-0040's Option B).** Not an
  alternative here — it would make Option C cleaner (`clear_pending_commit`,
  `openmls-0.8.1/src/group/mls_group/mod.rs:374`, would discard the loser's staged
  commit with no snapshot restore at all) but it is an invariant-#3 change that
  ADR-0040 already weighed and deferred. Noted as the refinement that would retire
  Option C's retained-snapshot machinery if a future arc reworks the commit FFI
  anyway.

---

## Consequences

- **Positive (Option C).** The silent permanent fork becomes an automatic bounded
  retry, and its worst case becomes a **visible** error rather than a mystery. No
  `lakko-mls` change (invariant #3 untouched), no FFI change, no wire change, no
  server change, no schema change, no new attack surface. The rollback reuses the
  whole-provider restore whose safety argument is already written down and already
  reviewed. The Welcome-ordering constraint closes a *worse* failure (a phantom
  three-party fork) that exists latently today.
- **Negative / new risks (Option C).** A second durable snapshot per in-flight
  commit on every client (transient — dropped on the verdict). One extra journal
  fetch on the interactive membership path. Two unresolved retry mechanics (the
  add-path KeyPackage, the approve-path stale proposal ref) that the
  implementation must answer concretely, not paper over. A retry budget that
  bounds but does not eliminate livelock in a hot group. The race itself remains;
  the loser's dead commit remains in the journal for its TTL.
- **Neutral.** The steady-state (no race) path is today's behaviour plus one
  retained blob, one extra fetch, and a deferred second publish. Peers observe no
  change at all.

### Invariants touched

- **#1 (no plaintext PII on the server) — unaffected** by every option. The
  retained snapshot is client-local and device-key-wrapped exactly as the existing
  one is.
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
- **#4 (no fallback to non-E2EE / fail-closed).** **Strengthened by Option C**: a
  device never remains silently on an epoch the group did not adopt, and a Welcome
  is never published for a group state that no member holds. There is no
  confidentiality break in the current bug — it is an integrity/availability
  divergence — so no E2EE guarantee is relaxed by any option. **Option A's
  rejection is fail-closed by construction** (refuse the publish), but its *failure
  mode* is the permanent freeze, which is availability, not safety.
- **#5 (the social graph is not materialised on the server).** **Option C:
  untouched.** **Option A: a small erosion** — a per-group `current_epoch` row is
  new per-group server state (a commit-rate signal per group) where today the DS
  holds only opaque rows.
- **Invariant-index touchpoints.** This ADR proposes **no** new indexed
  `I-` / `C-` / `T-` entry. It resolves **G-38 residual 1**, which is a free-form
  known-gaps entry, matching how G-35 / G-36 / G-37 were kept. The invariant-index
  Quick-stats counts are unchanged. **The maintainer is editing
  `docs/security/known-gaps.md` and `invariant-index.md` in parallel; this ADR
  deliberately does not touch either.**

**Flag through `lakko-security` before merging any option.** Option C touches the
MLS commit publish ordering and the ADR-0040 durability argument. Option A touches
the zero-trust server's behaviour and adds an availability attack surface. Option B
touches the audited MLS state machine and relocates ADR-0022's authorisation
boundary. None of the three is a "no review needed" change.

---

## Open questions

- **The add-path retry and KeyPackage consumption.** `add_member` consumes one of
  the joiner's published KeyPackages. Does a discarded commit release it, must the
  retry re-fetch from discovery, and what happens when the joiner has none left?
  **Must be answered before implementation, not during.**
- **The approve-path retry and the stale proposal ref.** ADR-0027's pending
  external joins are epoch-bound and in-memory. After the replay the retained
  proposal is stale; ADR-0028's joiner retries on a stale epoch, so recovery
  exists, but the retry is not self-contained. Does the approve path retry
  automatically once the joiner re-proposes, or does it surface as
  "request expired, ask them to try again"? A UX question as much as a mechanism
  one — route through `ux-ui`.
- **The retry budget and its user-visible exhaustion state.** How many, with what
  backoff, and what exactly the user sees. Route through `ux-ui`.
- **Whether Option C generalises to the DeviceGroup and the channel
  PublisherGroup.** The DeviceGroup's commits go to the same journal wrapped in the
  ADR-0015 envelope, and ADR-0028 OQ3's co-publisher approve is a `PublisherGroup`
  commit. ADR-0040 explicitly scoped itself to the four chat paths and noted the
  same parity question; this ADR inherits it.
- **Whether Option C′ ships regardless.** It composes with every option and is
  cheap; the only reason to hold it is to avoid implying the hazard is closed.
- **Whether `epoch_of_message` should gain a `content_type` sibling on the FFI.**
  Option C's verdict step needs "is this row a Commit framed at N", and today only
  the epoch is exposed (`crates/lakko-ffi-uniffi/src/lib.rs:3475`,
  `crates/lakko-ffi-wasm/src/mls.rs:3458`). Epoch alone is a *sufficient* signal for
  the verdict when combined with "this row is not one of our own two artifacts",
  but a `contentTypeOfMessage` peek would make the check exact rather than
  inferential. A small, purely additive FFI addition — worth deciding explicitly
  rather than by omission.
- **The stale ADR index.** `docs/adr/README.md` carries a duplicated `## Index`
  section and both copies stop at ADR-0015. Out of scope here; flagged for a
  separate docs pass.

---

## Sprint decomposition (if Option C is picked)

- **S0 — UX.** The retry-exhausted error state, the approve-path
  "request expired" state, and whether a membership commit shows a pending
  affordance at all. `ux-ui` first, per the repo's UX-first rule.
- **S1 — resolve the two retry mechanics.** The add-path KeyPackage question and
  the approve-path proposal-ref question, answered in code before any client work.
  Possibly a small additive FFI peek (`contentTypeOfMessage`).
- **S2 — web reference.** Retained epoch-N snapshot inside the existing
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
