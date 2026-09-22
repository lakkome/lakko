---
name: audit-protocol
description: External-auditor review of Lakko's group protocol and MLS state machine — the proposal/commit lifecycle, the three-layer authorisation rule and whether its layers compose, the staged sweep and its rollback, committer election, role tables, external join, multi-device leaves, and state reachability (can a group reach a state it can never leave?). One workstream of an audit engagement; also usable standalone to review a protocol state machine for wedge states and authorisation composition.
context: fork
agent: general-purpose
---

# Audit workstream — protocol and group state machine

You are the protocol reviewer on an independent team. Your subject is the primary
reason this engagement exists: a group-membership state machine layered on RFC 9420
MLS, whose authorisation rules were each reasoned about locally and whose
**interactions** have not been.

Engagement: $ARGUMENTS

## Read these first

Two documents bind every auditor on this engagement. Read them before you read any
Lakko source, whether you were dispatched by `audit-lead` or invoked directly:

- [`.claude/skills/audit-lead/methodology.md`](../audit-lead/methodology.md) — the
  phase model and the four rules (code first, `path:line` evidence, measured or
  silent, read-only).
- [`.claude/skills/audit-lead/severity.md`](../audit-lead/severity.md) — the finding
  file format, the confidence values, and the severity rubric.

If no engagement directory was given to you, create one before you start:
`docs/audit/$(date +%F)-<slug>/findings/`, and record the frozen commit
(`git rev-parse HEAD`) at the top of your note.

## Your stance

Build the state machine yourself, from the code, before you read ADR-0045. The ADR
is a long and careful document and it will install its authors' model in your head;
that model is the thing under test. Read `crates/lakko-mls/src/group.rs`,
`group_proposals.rs`, `group_roles.rs` and `refusal.rs` first and draw the machine.
Then open the ADRs and record where your machine and theirs differ.

Two things the vendor tells you, which you should take as a starting point and not
as a boundary:

- An internal review of this exact surface returned three HIGH findings and a
  confirmed impersonation, **four of which needed no attacker at all**.
- One of the conditions that wedges a group permanently is **a KeyPackage expiring
  while a request waits** — i.e. elapsed time, against a request that by design has
  no deadline.

The interesting defects here are not attacks. They are ordinary sequences.

**Read-only.** Nothing outside `docs/audit/<engagement>/`.
**`path:line` or it did not happen.**

## 1. Build the machine

Produce an explicit model before analysing anything:

- **States.** Per group, per device: epoch, pending proposal set, staged sweep
  present/absent, membership active/inactive, durable snapshot vs journal cursor,
  outbox entries un-acked. Name every piece of state that persists across a restart —
  those are the ones that can wedge.
- **Transitions.** Every operation that moves state: propose, receive-proposal,
  stage, merge, abandon, receive-commit, send, restore, sweep-at-restart. For each:
  what it requires, what it writes, what it writes *durably*, and in what order.
- **The orderings that are load-bearing.** Where two writes must happen in a
  specific order for a property to hold, and what enforces the order — a comment, a
  function boundary, a lock, or nothing.

Then enumerate, mechanically:

- **Which states have no exit?** For each, what puts a device there, and can it be
  reached without an attacker? This is the highest-yield question in this workstream.
- **Which states are invisible from the UI?** A wedge the user cannot see is worse
  than one they can, because nobody reports it.
- **Which states differ between two devices that have seen the same journal?** Any
  divergence that persists is a fork.

## 2. The authorisation rule and whether it composes

`crates/lakko-mls/src/group_proposals.rs`. One rule, consumed at three layers:
receipt gate, sweep filter, peer-verified commit gate. By-reference proposals are
judged against the proposer; by-value against the committer.

The vendor's own first question is whether these compose, and says the compositions
are untested. Test them on paper, then in code where you can:

- A commit carrying **both** a by-reference and a by-value proposal, where the
  authorisations differ.
- A proposal authorised at receipt whose **author is demoted** before the sweep.
- A sweep that **drops one proposal of a pair** that only make sense together — a
  role change naming someone an owner, and the `Add` that puts them in the group.
- Two proposals that are individually fine and jointly unbuildable.
- A proposal that becomes authorised, or unauthorised, because of an *earlier
  proposal in the same commit*. Order-dependence inside one commit is where
  layered rules usually break.
- The **self-service** arms (a member acting on its own account's devices): does the
  "own account" test rest on something the actor controls?

For each composition, answer: which layer decides, what does it decide against
(pre-merge or post-merge state?), and do all peers decide the same way. A rule
evaluated against different state on different peers is a fork, not a refusal.

## 3. The sweep lifecycle and its rollback

`stage_pending_proposals` / `merge_staged_sweep` / `abandon_staged_sweep`.

- A staged sweep **freezes that group's membership operations and is durable.** Walk
  every path from staging to a verdict and find the ones that do not reach one:
  a crash, a process kill, an exception, an early return, a network failure with no
  catch, a user closing the screen. Each is a durable wedge across restarts.
- The rollback is the *absence* of the eager merge. Confirm there is genuinely no
  other writer that advances the epoch, and that a sibling group's state cannot be
  rolled back with it.
- **The verdict read itself.** A device decides it won or lost by reading the
  journal. What does it read, and can that read be wrong — a stale page, a partial
  batch, another member's ordinary message, a row written between two reads?
- The pending-set park around `create_application_message`: what happens if the
  unpark fails, and what happens if a second send interleaves while the set is
  parked?

## 4. Concurrency and ordering

The delivery service orders by arrival, not by epoch, and the second actor is
always a different device — so no local lock can be the answer.

- Two committers at one epoch: who loses, does the loser know, does it recover.
- A local operation in flight while a remote commit lands.
- Two role changes in flight (RFC 9420 admits one `GroupContextExtensions` proposal
  per commit — what happens to the second, on the proposer and on every peer).
- A message sealed at epoch N published after the epoch moved: is it late or is it
  permanently undecryptable? Check what `max_past_epochs` is actually set to.
- Restart in the middle of any of the above.

For each: is the outcome *convergent* (all devices end in the same state), merely
*safe* (nobody is wrong, someone is stuck), or *divergent*?

## 5. Liveness

The design deliberately gives a membership request no deadline: it waits for a
committer. Examine what that costs.

- The committer election (`committer_rank`): is it a total function of authenticated
  state on every peer, or can two peers rank differently?
- The liveness beacon: what does a receiver do with a beacon whose sender it cannot
  rank, whose timestamp it cannot parse, or that arrives after the sender went away?
  Does "hearing nothing" mean act or defer, and is that the safe direction?
- Can an actor with a legitimate role deny liveness indefinitely — fill a queue,
  hold a sweep, churn a role? Bound each one honestly: an admin who can already
  remove members is not escalating by being annoying, but an admin who can remove
  *the means of repair* is.

## 6. Identity and authorship through membership change

The internal review's confirmed impersonation was here: an author was identified by
**leaf index**, and MLS hands a vacated leaf to the next member added.

Check every place an identity is keyed, and what it is keyed *on*:

```bash
grep -rn 'leaf\|sender_leaf\|leaf_index' crates/lakko-mls/src apps/web/src/chat apps/ios/Lakko/Chat apps/android/app/src/main/kotlin/me/lakko/app/chat --include=*.rs --include=*.ts --include=*.swift --include=*.kt | head -60
```

A leaf index used as a durable key is a defect until proven otherwise. A signature
key resolved **at receipt** is correct; resolved later, against a roster that has
moved, it is not. Check read receipts, reactions, edits, deletes, typing state,
mute/block lists, push routing and any cache keyed per member.

Then the multi-device layer: a conversation leaf is a *device*, and the account key
names the *person*. Every rule that should apply per-person but is implemented
per-leaf (or the reverse) is a finding.

## 7. External join and admission

- What does a link-holder who is not a member actually get, and what stops them
  consuming a resource that members need (a queue, a quota, a rate)?
- The approval gate: is the refusal peer-verified, or only enforced by the approver?
- Can a stranger's artefact reach a member's state before any authorisation runs?

## 8. RFC 9420 conformance at the wrapper boundary

Lakko wraps OpenMLS rather than implementing MLS. So the question is not "is MLS
correct" but **where does the wrapper's model of MLS differ from MLS's own**:

- Every place Lakko re-implements a check OpenMLS already does, or assumes one it
  does not. The vendor's C-66 enumerates fifteen `CommitBuilder::build` failure
  conditions — a *mirror of rules Lakko does not own*. Check the mirror against the
  pinned openmls version's source, and say what happens to each condition the mirror
  missed.
- Every place a Lakko rule could contradict an MLS rule, so that a commit one peer
  builds another must reject.
- Validation the wrapper skips because "the DS is trusted for ordering" or "peers
  re-verify anyway" — find the statement, then find the peer that actually re-verifies.

## Findings

[`severity.md`](../audit-lead/severity.md) format, numbered **`LAK-PR-NN`** (your workstream's filing prefix — `audit-verify` renumbers to the report id at consolidation, so never reach for a bare `LAK-NN`), under `docs/audit/<engagement>/findings/`. Then `11-protocol.md`:
your state machine, the composition analysis, the wedge-state enumeration, the
conformance notes, and — explicitly — the vendor's ranked questions 1, 2 and 3 with
Answered / Partially answered / Not reached.
