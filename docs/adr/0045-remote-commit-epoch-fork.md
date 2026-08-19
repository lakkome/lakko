<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0045: Closing the remote-commit epoch fork — proposal-based commits with a designated committer (G-38 residual 1)

- **Status:** **Proposed.** No code. **The maintainer has chosen Option B —
  proposal-based commits.** This ADR was originally written as a recon that
  recommended Option C (client-side loser rollback); that recommendation has been
  **overturned by the maintainer** and this revision records the choice, states
  the reasoning that moved it, and settles the five design questions the recon
  left open. The recon of every option is kept intact below, because its value is
  the record of *why* the rejected options are rejected. Status stays **Proposed**
  until the maintainer accepts the settled design; the maintainer accepts, not the
  ADR.
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
standalone option**, but **one component of it survives and is required** — the
bounded committer rollback of decision 2 below. Option C′ (re-read the journal head
before committing) is **absorbed into B** as the committer's stand-down rule, not
shipped as a separate companion.

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

#### The design question this leaves, stated as a trade and not decided here

**Variant 1 — a candidate heartbeat, and the maintainer's model becomes literal.**
Each owner/admin device publishes a small periodic liveness signal for the group
while the app is foregrounded. Then "active admin devices elect the committer" is
implementable as written: each candidate computes the active set from beacons seen
within a freshness window, and the lowest-ranked active candidate sweeps. **The
stagger, and its constant, disappear entirely** — nobody waits a turn, because the
set is known rather than inferred. It is not built, but it is **well-precedented
rather than blocked**: typing indicators are the working proof that a throttled
ephemeral per-group journal signal with an expiry window functions, and
`lakko-presence` exists for exactly this shape.

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
4. **The residual is repaired, not prevented.** If two candidates do collide, one
   forks — the hazard this ADR exists to close, in a much narrower window. So the
   acting committer **retains its pre-commit epoch-N snapshot across the
   publish→verdict window and rolls back if it lost**: Option C's steps 1, 3 and 5,
   on one device, on the non-owner path only. Required in both variants.

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
  other (decision 4).

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

**Ownership transfer is being built.** The replaced design listed ADR-0022's
deferred ownership transfer as a follow-up that B promoted from a nicety to
load-bearing. The maintainer has since chosen to **build it** rather than ship the
owner dead-end it would otherwise leave: the core refuses an owner `Leave` /
self-`Remove` fail-closed (`crates/lakko-mls/src/group_proposals.rs` — *"the group
owner cannot leave"*), and `set_group_roles` is owner-only, so an ownerless group
could never appoint another admin and an owner is therefore bound permanently to
every group they create. A core sprint for it is running now; its API is not
described here because this ADR does not yet know it.

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
   supporting facts make this the right layer to drop at: `ProposalStore::remove`
   is `pub(crate)` in openmls, so there is **no public single-proposal eviction** —
   once stored, a proposal can only be cleared with `clear_pending_proposals`,
   which clears *everything* — and openmls empties the store on every merged commit
   (`staged_commit.rs:549-552`), so no bespoke GC is needed for the ones that are
   legitimately stored.
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
   nothing expires and nothing is declared unachievable. The maintainer has chosen
   to **build** ADR-0022's deferred **ownership transfer** rather than leave the
   owner dead-end this exposes; a core sprint for it is running.
3. **A new authority shift: the committer can selectively censor.** An elected
   committer that sweeps some proposals and drops others is not detectable except by
   each proposer noticing its own proposal never landed. Today an admin acts
   unilaterally and cannot be filtered by a peer. Bounded by the fact that another
   candidate becomes the acting committer once this one stops acting — so a
   committer that sweeps *nothing* loses the duty — but **partial** censorship by an
   otherwise-active committer is not bounded by anything in this design. Named, not
   solved.
4. **Proposals go stale too.** ValSem003 rejects *any* non-Application message at
   the wrong epoch, not only Commits, so a proposal that misses the sweep must be
   re-proposed. This is cheap — the whole point — but it is a client state machine,
   not a no-op, and a stale proposal litters the journal for its 30-day TTL the way
   the recon says a dead commit does under C.
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

---

## Blast radius, per sprint

| Sprint | Surface | What changes |
|---|---|---|
| **S0 — UX** | none (design) | The five states of decision 5 plus the committer-side pending surface. `ux-ui` first per the repo rule. Landed as [`docs/ux/proposal-based-membership.md`](../ux/proposal-based-membership.md), out of order — after S1+S2 rather than before them. |
| **S1 — core: the gate** | `crates/lakko-mls` (**invariant #3**) | `process_incoming_verifying_roles` proposal arm: inspect + role-check + `store_pending_proposal` or drop. `authorise_role_commit` extended from committer-only to per-proposal-sender, with the by-value arm preserved and a `SelfRemove` arm added. New `Incoming` shape for a stored-vs-dropped proposal. Ten property tests. **External crypto review.** |
| **S2 — core: the committer** | `crates/lakko-mls` | `committer_rank` in `group_roles.rs`; a `sweep_pending_proposals` entry point over `CommitBuilder::build` with the two-arm filter; `propose_add` / `propose_remove` / `propose_role_change` wrappers over the openmls `propose_*` calls. |
| **S3 — FFI** | `lakko-ffi-uniffi`, `lakko-ffi-wasm` | Add the propose + sweep + rank + pending-proposal-list surface beside the existing `add_member_from_wire` / `remove_member` / `set_group_roles` / `approve_external_join`. Both surfaces, in lock-step. Regenerate bindings, rebuild `LakkoCore.xcframework` and the Android `.so`. |
| **S4 — web reference** | `apps/web/src` | `contacts/client.ts`: the four `…Locked` methods become propose-and-publish; a new sweep path. `mls/group-lock.ts` `withGroupMlsLock` still spans it. `chat/client.ts` `applyPolledBatch` / `toEvent`: the `Proposal` result stops being a content-less system row. `chat/commit-outbox.ts` + `chat/mls-store.ts` `saveSnapshotWithCommitEntry`: the retained epoch-N snapshot for the committer path. `routes/Chat.tsx` members panel: the new states. |
| **S5 — iOS mirror** | `apps/ios/Lakko` | `ChatRepository.swift` (`withCommitLock`, `persistCommitEntryThenSnapshotOrThrow`), `ChatClient.swift` (`applyFetchedMessages` / `toEvent`), `CommitOutbox.swift`, `UI/Chat/MembersSheet` + `ChatViewModel`. |
| **S6 — Android mirror** | `apps/android/.../app` | `ChatRepository.kt` under the two-tier `groupLock` + `providerLock`, `ChatClient.kt`, `CommitOutbox.kt` (`prepareCrashSafeCommit`), `ui/chat/MembersSheet` + `ChatViewModel`. |
| **S7 — external-join convergence** | core + FFI + 3 clients | Decision 4: retire `pending_external_joins`, keep the FFI names. |
| **S8 — docs** | `docs/` | Flip this ADR's status; reconcile `known-gaps.md` G-38 residual 1 and `invariant-index.md`; CLAUDE.md ADR-table row + `lakko-mls` module-map row. |

**The table does not yet scope decision 2's variant 1.** A candidate heartbeat is a
new ephemeral in-group signal — an envelope kind in `lakko-contacts`, both FFI
surfaces (`lakko-presence` is exposed on **wasm only** today, with no UniFFI
counterpart), and a beacon-plus-freshness fold on each of the three clients beside
the existing ADR-0024 typing fold. It is a sprint of its own, sequenced before or
alongside S4, and it is not costed here because the maintainer has not yet chosen
between the two variants. **Variant 2 needs no new sprint** — the stagger is
client orchestration inside S4–S6.

**The outbox/lock layer is the least symmetric surface and will cost the most per
platform.** Web has `withGroupMlsLock` plus an atomic IndexedDB transaction and no
single create+persist+publish wrapper; iOS has `withCommitLock` (a `@MainActor`
task chain) plus an ordered, non-atomic two-step persist; Android has the two-tier
`groupLock` + `providerLock` with `prepareCrashSafeCommit` inside the provider lock
and the publish outside it. Any change to the commit sequence has to be expressed
three different ways — budget for that rather than assuming the web reference
translates.

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

- **Decision 2's variant choice — a candidate heartbeat, or a staggered takeover?**
  **The live one, and the maintainer's.** Variant 1 makes "active admin devices
  elect the committer" literal and removes the last timing constant, at the cost of
  multiplying an anti-pattern ADR-0024 already accepted grudgingly (continuous
  journal rows, retained to the 30-day prune) and of publishing each candidate's
  activity schedule to the whole group. Variant 2 needs no new signal but keeps one
  unmeasured stagger interval. Recommendation in decision 2: build variant 1, keep
  variant 2 as the fallback. **Neither is decided here.**
- **~~The retry budget and its user-visible exhaustion state.~~ Answered — there is
  no retry budget.** The revised decision 2 removes both the cap and the terminal
  failure it produced: a request waits indefinitely rather than exhausting a budget,
  and the committer's own rollback needs no budget because it replays no intent
  (the proposals are the durable record of intent, and the winning commit consumed
  them). Recorded here rather than deleted because the UX spec was written against
  the budget and had to be reconciled to its removal.
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
