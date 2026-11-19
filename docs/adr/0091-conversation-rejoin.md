<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0091: A device that fell behind a conversation notices it and comes back

- **Status:** Proposed (2026-11-18). The direction is the maintainer's
  (2026-11-18: "a device that fell behind a conversation must notice by
  itself and come back, like ADR-0081 does for the DeviceGroup, on every
  client"); the maintainer decided five points the same day (see
  "Maintainer decisions"), the main one being the order: **phase 1 prevents
  a device from dropping out, phase 2 brings back one that did**. Phase 1
  (prevention) is built on web (incl. desktop), iOS and Android; phase 1b
  (the DS names the registered commit row, a snapshot rolled back under its
  cursor reads the commit again, the web DeviceGroup loop holds its commit,
  identifier-free log lines) was built the same day on `services/ds` and the
  three clients, host-tested only (section "Phase 1b, as built"); phase 2
  (recovery) S0 (spike), S1 (core) and S2 (both FFI surfaces and the NSE
  surface) were built on 2026-11-19, host-tested only (section "Phase 2
  S0-S2, as built"); S3 (web and desktop), S4 (iOS and Android) and S5 (live)
  are not built, so no client drives the recovery yet.
- **Date:** 2026-11-18
- **Context:** proposal-based membership (ADR-0045), per-device leaves
  (ADR-0050), journal authorisation and ownership (ADR-0053, ADR-0057),
  DeviceGroup rejoin (ADR-0081), conversation kind (ADR-0062), knock key
  (ADR-0087 section 5), servers that cannot link accounts (ADR-0078)
- **Amends:** ADR-0045 (a new authorisation arm: a device's own re-entry
  replacement; C-88 gains one more case a Welcome may replace), ADR-0050
  (a device whose leaf is still in the roster can be brought back), ADR-0057
  (journal repair is held while this device is behind with evidence of real
  progress), ADR-0062 (a one-to-one leaf can be replaced, never removed)
- **UX spec:** [`docs/ux/conversation-rejoin.md`](../ux/conversation-rejoin.md)

## Context

### What happened (verified 2026-11-18)

The macOS desktop app (the web client in Tauri) is a member of the
role-table group `c14ae3b45bdb6a66...`, created on the test stack on
2026-11-11 at 12:25. Its local MLS group is at epoch 6. The test-stack DS has
epochs 0 to 16 registered for that journal; the commit that moved the group
from 6 to 7 is DS row 7967 (2026-11-11 13:10). The desktop never applied it.
Why is unknowable now: there is no log of that day.

From then on:

- every later row fails to process (it is sealed at an epoch the device does
  not have), and is skipped;
- `ProfileContext` republishes the profile into the group about every 30 s
  and the DS answers `401 proof required` (the epoch-6 key is no longer in the
  write tier);
- `BackgroundReceiveContext` logs "ADR-0050 re-admission: retire not
  proposed ... 401";
- the person sees a chat that went silent on 2026-11-11 with nothing
  explaining it, and every send fails;
- `isActive` stays true: the device believes it is a member at epoch 6.

### Why no client can notice (recon, file:line)

The same shape exists on all three clients:

- **Web** `apps/web/src/chat/client.ts:1528-1563` (`applyPolledRows`): the
  cursor is set to the row id before processing; a row whose
  `processIncomingVerifyingRoles` throws is logged (`console.warn`) and
  skipped, "deliberately: a row that can never apply must not be able to
  wedge the poll". The sweep verdict reader `contacts/client.ts:954` skips
  the same way.
- **iOS** `apps/ios/Lakko/Chat/ChatClient.swift:491-499`: drop and advance,
  no log; `Chat/CommitterSweep.swift:1078` `try? group.applyIncoming(row)`.
- **Android** `chat/ChatClient.kt:439-457` (catches every `Throwable`, no
  log); `chat/CommitterSweep.kt:1018-1032`. `chat/DurableCursor.kt:24-29`
  already states the consequence: "the device sits at epoch N while the group
  is at N+1 - silently, for every subsequent message ... Only a
  remove-and-re-add recovers it."

The signal that would tell a device it is behind exists and is unused for
conversations: a signed read of a claimed journal carries `latest_epoch`
(`services/ds/src/access.rs:370-418`), decoded by all three DS clients (web
`ds/client.ts:208-218`, iOS `DS/DsClient.swift:1041-1051`, Android
`ds/DsClient.kt:1187`) and read only by the DeviceGroup and channel code.

Since ADR-0081 an epoch is registered on the DS **together with** the commit
row that reaches it, in one transaction (`POST /ds/group/{hex}/commit`; a
plain `/access` Append is refused). So a read that reaches the end of the
journal and still names a `latest_epoch` above the device's own epoch means
the device has read, and failed to apply, the commit that got there. That is
the fact this ADR builds on.

### Why the existing recovery paths do not help

- **ADR-0081** covers the DeviceGroup only (`isBehind`: web
  `devices/device-group-rejoin.ts:180-185`, iOS
  `Devices/Sync/DeviceGroupRejoin.swift:157-162`, Android
  `devices/sync/DeviceGroupRejoin.kt:180-183`; rejoin by a knock sealed under
  the lineage seed, answered by any foreground sibling with a remove-and-add
  covered by a self-removal proof, `lakko-mls` `self_removal.rs`,
  `DeviceGroup::replace_device_at`). Its rules are a template, but its
  carrier (a key only the account's own devices hold) and its gate (the
  ADR-0015 attested-removal policy) are DeviceGroup-specific.
- **ADR-0050 re-admission** compares leaf signature keys with directory
  device keys (web `chat/device-readmission.ts:85-94`, iOS
  `Chat/DeviceReadmission.swift:99,109`, Android
  `chat/DeviceReadmission.kt:91`, re-checked under the lock with
  `skipIfPresent`). A device that fell behind keeps its device identity, so
  its key is in the roster and it is neither admitted nor retired. It is
  invisible to that arm by construction.
- **A Welcome cannot replace the stale group.** `Group::join_from_welcome`
  refuses a stored group that is still active (`lakko-mls` `group.rs:955-966`,
  `Error::GroupStillActive`, C-88). Worse, the clients' held-Welcome pass
  DROPS such a Welcome once the journal has been read to the end (web
  `chat/membership-end.ts:224`, iOS `Chat/MembershipEnd.swift:81-87`, Android
  `chat/MembershipEnd.kt:592-596`), and a device that skips every row always
  "reads to the end". So even a member who re-added the device by hand would
  have its Welcome thrown away.
- **Authorisation forbids the obvious repair in two common cases.**
  `group_proposals.rs:408-420`: the owner's last leaf is unremovable by
  anyone, the owner included; `group_proposals.rs:822-829`: in a one-to-one a
  person's last leaf is unremovable. A group created on the desktop with the
  desktop as the owner's only device in it is exactly the first case.

### A hazard found on the way (code reading, not tested)

`JournalRepair` (web `ds/journal-repair.ts:20-31`, iOS
`DS/JournalRepair.swift:154-187`, Android `ds/JournalRepair.kt:186-238`)
lets an owner or admin device whose signed request met `401` at its own
epoch N, and which is "still at N" after polling to the head, send
`Replace(N, keyAtN)`, which drops every epoch above N that a strictly lower
rank registered. An owner or admin device that fell behind polls to the
head, stays at N (it skips the commit), and so concludes the epochs above
are fake. If the commits above N were registered at member rank (a
self-service device admission swept by a member), its Replace would drop the
real chain from under every other device. This ADR holds that repair while
the device has evidence the epochs above are real (section 2).

## Maintainer decisions (2026-11-18)

1. While a device is out of a conversation, the composer is hidden.
2. No time limit: the device waits to be brought back. There is no "Remove
   from this device" after a week and the waiting note never changes to it.
3. Missed messages are sent by the device that brings it back: another
   device of the same account in that conversation if there is one;
   otherwise the owner's or admin's device that re-adds it sends the
   messages it holds for the gap (the account was a member throughout, so
   nothing new leaks). The "can't be shown" line stays only for what none of
   them holds (older than the DS retention, or not on the re-adder).
4. In a group with no other device of the account, the owner or an admin
   re-adds.
5. **Order: "the very fact of dropping out is wrong."** First prevent a
   device from dropping out (phase 1), then recover one that did (phase 2,
   the safety net).

## Phase 1: a device does not skip the commit that moves its conversation

### The rule

**Superseded by phase 1b (the exact rule below in "Phase 1b, as built"):** a
failing row is held exactly when the DS names it as the registered commit
leaving this device's epoch. The framed-epoch rule of this section is what
phase 1 first shipped and is kept as the record of it.

When a row fails to apply, the client classifies it before skipping it. It
reads, without decrypting, the row's framed epoch (`epochOfMessage`) and the
epoch after it (`epochAfterMessage`; greater only for a commit), its own
group's epoch before the row (`local`), and the DS's `latest_epoch` from the
same read:

```
hold  := sealed == local                 -- framed at this device's epoch
     and after  == local + 1             -- a commit
     and latest_epoch >= local + 1       -- the DS registered the next epoch
skip  := anything else
```

Since ADR-0081 an epoch is registered on the DS only together with its
commit row, and a losing commit is never appended, so the commit framed at
`local` in a journal whose registered head is above `local` is the
registered commit for `local + 1`. Rows the DS did not register as the next
epoch (an application message of an older epoch, a commit of an epoch this
device is past, a commit of an epoch beyond the next, a commit while the DS
names no later epoch) are skipped as today, so a row that can never apply
still never wedges the poll.

**Hold** means: the row is not skipped. The batch stops at it, the cursor
stays before it (rows after it are read again with it), and the apply is
retried on later polls with a bounded backoff (2 s, 5 s, 15 s, 30 s, 60 s,
120 s, 300 s after the failures; chosen, not measured). While a hold waits
for its next attempt, a poll of that conversation does not fetch: it waits
out its long-poll time or the attempt time, whichever is first, so the hold
never spins. A restart reads the journal from the held position and so
retries at once.

When the attempts are spent (8 attempts, chosen) the row is skipped as
today, the client logs loudly that this device is now behind in that
conversation, and keeps a mark of it (the conversation's epoch and the time)
for phase 2. Phase 1 alone cannot bring it back.

**Why a retry can succeed at all.** The bytes do not change, so a retry
succeeds only if the device's state did: a staged sweep of its own resolved
by its verdict, a handle reloaded from the durable snapshot after a crash,
another path (the sweep verdict reader, the background receive) having
applied something in between. The live case of 2026-11-18 left no log, so
which of these, if any, caused it is unknown; phase 1 guarantees that the
next such failure is retried, recorded and visible instead of silent.

**A registered commit this device refuses by its own rules** (the role
gate, the proposal gate, the one-to-one rule) while the DS registered it as
the next epoch means the other devices applied a commit this one will not:
a bug or version skew between clients. It is held and retried like any
other failure and logged at error level as a divergence; it is never skipped
silently.

**What is logged.** One plain line per failed apply on all three clients,
with no identifier and no content: the failure kind (a fixed set: refused
with the typed refusal code, wrong epoch, own message, missing proposal,
decrypt failure, other), the row's framed epoch and epoch after, the local
epoch, the DS's latest epoch, the verdict (hold or skip) and the attempt
number. Never a group id, row id, key, account, name or text. iOS and Android
logged nothing on a failed apply until now.

**Who else consumes rows.** The ADR-0045 sweep verdict reader applies the
rows the DS ordered below this device's own commit and skips a failure. It
logs with the same line but does not hold: since ADR-0081 the verdict is the
DS's answer to the commit route, and a registered commit cannot sit below
this device's own registered commit (one commit per epoch). Journal repair (ADR-0057) does not Replace
while the conversation holds a registered commit or carries the behind mark:
the epochs above are then real (see "A hazard found on the way").

**The DeviceGroup** already holds a failed commit on iOS and Android
(ADR-0081, `FailedRowVerdict`: the persisted cursor stays before a commit
framed at or above the local epoch). The web DeviceGroup loop
(`devices/sync/journal-batch.ts`, rule 1) did not; phase 1b aligned it and
moved all three to the exact rule (open question 7, closed). The channel PublisherGroup loops get
the same identifier-free log line where they skip silently.

### What phase 1 needs from the server

(As first written; phase 1b made the server change, see below.) Nothing. The rule above identifies the registered commit from the framed
epochs and `latest_epoch`, which the DS already returns. It is exact for
honest devices; a member that posts a commit-shaped garbage row framed at the
current epoch through the plain publish route, while a real commit for the
same epoch also exists, can make devices hold at the garbage row until the
attempts are spent, then skip it and apply the real one. Exact
identification needs the DS to name the registered commit's row in a read
response (it stores `commit_message_id` with each registration since
ADR-0081): a server change, not made (open question 6).

### Not in this pass (built in phase 1b)

A durable snapshot rolled back under an already advanced cursor (a handle
writing an older snapshot) would also strand a device, and holding a row
cannot catch it: the cursor is already past. Recording with the cursor the
epoch it corresponds to, and rewinding to the commit that left the loaded
epoch when the snapshot is found below it, would. Built in phase 1b (below).

## Phase 1b, as built (2026-11-18)

Approved by the maintainer the same day: open question 6 (the DS names the
registered commit) and open question 7 (align the web DeviceGroup loop).
Host-tested only; nothing was run on a device, against the test stack or in
a live conversation, and nothing is deployed.

### 1. The DS names the registered commit row

A signed read of a claimed group journal (`GET /ds/group/{hex}/messages`
with a valid proof) gains one field beside `latest_epoch`:

```
"commits": [ { "epoch": N, "id": row_id }, ... ]
```

one entry for every registration ABOVE the epoch the read's proof was signed
at that carries a commit row (`ds_group_access.commit_message_id`, ADR-0081):
row `id` is the commit that moves the journal from `N - 1` to `N`. Ascending
by epoch, at most 64 entries (`MAX_RETAINED_EPOCHS`), present (possibly
empty) exactly where `latest_epoch` is, absent for an unclaimed journal and
a mailbox. It is read after the rows, so a commit row in `messages` always
has its entry (a registration and its row commit in one transaction). An
epoch registered without a row (claim, reset, Replace, which clears the
binding) is not named. Keyed on the proof's epoch, not on the returned
window, so a device whose cursor is already past a commit still learns it
(item 2). The DS learns nothing new: the proof's epoch was already in the
header, and the ids are its own. `services/ds/src/journal.rs`
(`registered_commits_above`), `access.rs` (`Gated::Proven.proof_epoch`);
test `a_signed_read_names_the_registered_commit_rows_above_its_epoch`
(DB-gated). Considered and not chosen: only the commit leaving the proof's
epoch (smallest, but a batch that applies one commit and fails the next
would not know the second); the commits inside the returned window (a cursor
past the commit learns nothing).

**The hold rule becomes exact.** A failing row is held exactly when it is
not this device's own echo and `commits[local + 1]` is its id; every other
failing row is skipped at once. A commit-shaped row a member posted through
the plain publish route is never named, so it no longer holds anyone (the
G-58 phase-1 clause is closed). Web `chat/registered-commit-hold.ts`
(`registeredCommitVerdict`, `registeredCommitLeaving`), iOS
`Chat/RegisteredCommitHold.swift`, Android `chat/RegisteredCommitHold.kt`;
the backoff, the attempts, the behind mark and the log line are unchanged.

### 2. A snapshot rolled back under its cursor

With each cursor advance of the conversation poll, made only after the
snapshot reached disk (C-67), the client records the group's epoch with the
cursor, keeping the highest (it never moves backward). When the poll finds
the group loaded below the recorded epoch, and the read names the commit
leaving the loaded epoch at or below the cursor, it reads again from just
before that commit (at most 20 pages, chosen) and applies the rows; the
cursor itself never moves backward, and rows already filed are filed again
by DS id. One line is logged, epochs only. A commit the DS no longer names
(past its retention, replaced) cannot be read again: no rewind, and the
device is behind (phase 2). A held commit on a rewound read is held as
usual. The record is written by the poll only; other cursor writers (the
sweep verdict reader) do not record, so the record can lag, which only means
fewer rewinds, never a wrong one.

- Web: `chat/cursor-epoch.ts` (`localStorage`
  `lakko.chat.cursor-epoch.v1:<account>:<group>`, dropped with the group in
  `membership-wipe.ts` and on Forget device in `wipe-account.ts`, writes
  through the ADR-0077 `Storage.setItem` gate), `ChatClient.pollMessages` /
  `fetchRewound`.
- iOS: `Chat/CursorEpoch.swift`; the epoch rides the message-log slot beside
  the cursor (`PersistedGroup.cursorEpoch`, same store, same wrap key, same
  wipe), recorded in the durable-cursor advance; `ChatRepository.pollGroup`
  and `ChatClient.fetchRewound`.
- Android: `chat/CursorEpoch.kt`; `GroupSettingsStore` holds the epoch per
  account and group (cleared with the group's settings and on Forget
  device, behind the store gate), recorded in the poll's advance;
  `ChatRepository.rewoundIfRolledBack`. The fetch-half rewind in
  `pollGroupOnMlsThread` has no test of its own (it needs a `Context` and an
  FFI group); its rule and the apply loop are tested.

### 3. The DeviceGroup (open question 7)

All three DeviceGroup receive loops use the exact rule: a failing row that is
the registered commit leaving the handle's epoch pins the PERSISTED cursor
before it for the rest of the run; the run reads on (a row that can never
apply must not wedge the loop), and the next start reads the commit again.
Every other failing row passes. iOS and Android `FailedRowVerdict` changed
from "a commit framed at or above the local epoch" to this rule (an
unreadable handle epoch now passes on Android, where it held); web
`devices/sync/journal-batch.ts` gains rule 5 and `DeviceGroupSyncContext`
supplies the verdict from the read's `commits`.

### 4. Log lines without identifiers

Every line touched here prints no group id, journal id, row id, key, account
or endpoint, and no error text (a DS error's text names its endpoint, which
names the journal). A small error tag keeps the error type names along the
cause chain, a DS status and a typed refusal code: web `chat/log-tag.ts`,
iOS `Chat/ErrorLogTag.swift`, Android `chat/ErrorLogTag.kt`. Changed: the
ADR-0050 re-admission lines (check failed, add not proposed, retire not
proposed, and "N to admit, N to retire", which named the group) on all three;
the DeviceGroup row-not-applied lines (web printed the row id and the error,
Android printed `message.id` on two lines); web's knock read, knock row and
knock persist lines; Android's held-persist line (it printed the cursor).

## Phase 2: recovery

**A device that reads a conversation journal to its end and finds the
journal registered at a later epoch than its own, with no commit of its own
owed, is behind. It stops acting in that conversation, asks to be brought
back with a fresh KeyPackage under its own device identity that names the
conversation and the epoch, and is brought back by one commit that replaces
its old leaf with that KeyPackage. It keeps its signature key, so the person,
their role and their authorship are unchanged. The device that brings it
back also sends it the messages of the gap that it holds.**

### S0 result (2026-11-19)

A scratch crate outside the workspace, on OpenMLS 0.8.1 as pinned in
`Cargo.lock` (three members, one misses a self-update commit, a plain member
re-adds it). Observed:

- **M1 as written works.** A KeyPackage carrying `Extension::Unknown(0xFF04)`
  in its KeyPackage extensions builds, and an `Add` proposal carries it to
  every peer: the receiving member reads the marker from the queued `Add`
  (`add.key_package().extensions().unknown(0xFF04)`) at receipt and again
  from the staged commit. One requirement: `KeyPackageIn::validate` refuses
  the package (`UnsupportedExtension`) unless its own leaf advertises
  `0xFF04` in its capabilities. Nothing else is needed: the group's
  `required_capabilities` need not list it, and the other leaves need not
  advertise it. The fallback (a proof carried as a LeafNode extension) is
  not needed.
- **A colliding `Add` alone.** OpenMLS accepts a received `Add` whose
  signature key equals a current leaf's into the proposal store (key
  uniqueness is not checked at receipt), and refuses any commit carrying it
  without the matching `Remove`:
  `ProposalValidationError(DuplicateSignatureKey)` from
  `CommitBuilder::build`. So the Lakko gates must refuse it themselves, or
  it wedges the sweep (the existing `sweep_keeps` clause 3 already excludes
  it while the leaf stays).
- **The pair builds in one sweep.** `Add(K)` and then `Remove(L)`, both by
  reference from one plain member, commit in one `CommitBuilder::build`
  (`validate_key_uniqueness` seeds its sets from the roster minus the
  commit's removals). The peer sees both proposals with the proposer as
  sender. `K` took `L`'s leaf index.
- **Joining over the stale group.** Staging the Welcome over the stored
  active group fails with `GroupAlreadyExists` unless `replace_old_group` is
  set; with it, the staged roster shows exactly one leaf with the device's
  signature key, whose encryption key is `K`'s, and the Welcome's
  `EncryptedGroupSecrets` name `K`'s `KeyPackageRef`. After deleting the old
  group the device joins at the new epoch and decrypts the next message. A
  last-resort `K` stays in storage after the join (a one-time package is
  deleted while the Welcome is staged, before any check could refuse it).

### 1. Detection: suspected and confirmed

Two levels, because the cost of acting on each differs.

**Suspected** (stop acting, ask to come back). For a conversation group
(ordinary group or one-to-one) that is active on this device:

```
suspected :=
     not own_commit_owed               -- no ADR-0040 outbox entry and no
                                       -- staged sweep awaiting a verdict
  and (
       ( drained                       -- the read returned no row and no
                                       -- "more"; the cursor was not held
         and latest_epoch > local_epoch )
    or refused_reads >= 3              -- 3 consecutive 401 on reads signed at
                                       -- the device's current key, clock not
                                       -- skewed (its key left the read tier:
                                       -- over 30 days or 64 epochs behind)
  )
```

`local_epoch` is the group's own epoch. This is ADR-0081's `isBehind` and
its lost-by-refused-reads rule, applied to conversations, and it is written
once in Rust (section 6) instead of three times.

**Confirmed** (shown to the person, section UX 2.2):

```
confirmed := suspected and evidence
evidence  := since its last applied row the device failed to process a row
             framed at an epoch greater than local_epoch (someone is writing
             at a later epoch; the framed epoch is readable without
             decrypting)
          or a publish of this device to this journal was refused 401 with
             the clock not skewed
          or suspected has held over reads spanning 10 minutes
          or the suspicion came from refused_reads
```

**Cleared** by a drained read with `latest_epoch <= local_epoch`, or by the
re-entry landing (section 5).

**False positives considered.**

- *A device that was merely offline* reads the missed rows when it comes back
  and applies them; it is only suspected if an apply fails. Not a false
  positive.
- *A commit in flight.* Registration and row are one transaction (ADR-0081),
  and the DS computes `latest_epoch` before it fetches rows
  (`access.rs:404`), so a read that names epoch N+1 also returns or has
  already returned the row reaching N+1. A commit that lands after the gate
  shows on the next read. No timing window.
- *The device's own commit.* Excluded by `own_commit_owed`. After a crash
  that lost the echo record, a device that abandoned its own landed commit
  and then fails it as `CannotDecryptOwnMessage` is in fact behind; the rule
  calls that correctly.
- *A held batch* (persist failure) is not drained; the batch is re-read.
- *A fake epoch* (a member registrar appended an epoch with a garbage row,
  ADR-0057): every honest device is suspected, since none can tell a garbage
  commit from one it missed. Being suspected costs a request nobody can answer
  (no re-admitter is at the fake epoch, section 4) and a pause of the
  device's own commits and repeat envelopes. It is confirmed only with
  evidence; in the fake case nobody writes at the later epoch, so evidence
  arrives only by the 10-minute rule. The owner's or an admin's
  `JournalRepair` removes the fake epoch (it is allowed exactly when there is
  no evidence of real progress, section 2), after which the suspicion clears
  everywhere. Residual G-58.
- *A device removed from the group while offline* (its removal is the commit
  it cannot apply) is suspected and confirmed. No re-admitter will act (its
  leaf is gone from their roster), and the device waits with no time limit
  (decision 2). If the account left or was removed on another of its devices,
  the ADR-0087 section 9 `conversation_ended` register already ends the chat
  on this device first.

### 2. What a suspected device stops doing

In that conversation only, until cleared:

- no commit, no sweep, no committer beacon (C-64: it would otherwise fold an
  empty view, "hear nothing", and act), no proposal;
- no ADR-0050 re-admission arm, no ADR-0057 registration;
- `JournalRepair`'s Replace is allowed only without evidence (section 1):
  with evidence the epochs above are real and a Replace would cut them;
- no repeat envelopes (profile, group name, pins, push routing secret,
  registrar seeds, knock seeds), no typing or read receipts: each would be
  sealed at a stale epoch nobody can open;
- sends: while only suspected, a send is attempted as today (its `401`, if
  it comes, is evidence); once confirmed, the composer is replaced by the
  waiting note (UX 2.2) and nothing is sent.

It keeps reading the journal at its normal cadence (that is how it clears),
keeps its stale group state untouched, and deletes nothing. A wrong
suspicion therefore costs nothing that is not restored by clearing.

Skipping a row that cannot apply stays as it is: the poll must not wedge.
What changes is that each client records, per group, the first skipped row's
time, the highest framed epoch among skipped rows, and whether any was a
commit, which feed section 1 and the UX gap line.

### 3. The request to come back

The device mints a **re-entry KeyPackage** `K` under its own device identity
(the same leaf signature key as its current leaf `L`, the same ADR-0050
device binding), carrying a **re-entry marker** (section 6, M1) naming the
conversation's group id and `epoch_left` = the `latest_epoch` it read. `K` is
never published to the directory. The device records `{group, K's
KeyPackageRef, epoch_left, sent_at, route}` durably **before** sending, and
keeps the KeyPackage private keys for the last two requests per group.

The request is a new envelope, `lakko-contacts::conversation_reentry`:
`"lakko/conversation-reentry/v1\0" ‖ 0x01 ‖ group_id(32) ‖ epoch_left(8, BE)
‖ kp_len(u16, BE) ‖ K`, bounded and fail-closed, with a fuzz target. It goes
by the first route that applies:

1. **Another device of the account is a member of this conversation.** As a
   DeviceGroup service envelope. The DS sees an ordinary DeviceGroup row.
   This is the common case (the live desktop case, if the maintainer's phone
   is in that group).
2. **A one-to-one.** Into the other person's account mailbox, under the
   contact reach grant the device holds (ADR-0060; retired `contact` versions
   are still accepted and re-granted, 2026-11-06), as a new sealed mailbox
   kind. Sealed sender (ADR-0078): the DS learns nothing.
3. **A group.** Into the group's knock queue, sealed to the group's knock key
   (ADR-0087 section 5) of the newest version the device knows, with a kind
   byte that distinguishes it from a request to join. The owner and the
   admins hold the knock seeds (older versions stay openable). Today a plain
   member does not hold the knock public key; the owner therefore also sends
   `(knock_version, knock_pk)` to all members in-group (a small new envelope,
   re-sent on a version change and on growth, like the group name). One
   `ds-knock` token per request.

The device repeats the request every 30 minutes while confirmed (ADR-0081's
re-knock interval, chosen there, not measured), with a fresh `K` and the
current `latest_epoch`, and never more often.

### 4. Who brings it back, and the commit

A re-admitter is a device that holds the conversation at
`epoch == epoch_left`. It accepts a request only if all hold (written once in
Rust, section 6, M4):

- `K`'s KeyPackage signature verifies, and its marker names this group and
  this device's current epoch;
- a current leaf `L` has `K`'s signature key, and `K`'s device binding
  resolves to `L`'s account;
- that device is still the account's: for route 1 it is in the sibling's own
  DeviceGroup roster; for routes 2 and 3 the account's directory lists a
  device with that key (ADR-0074 removed devices are deleted from the
  directory and tombstoned);
- this device has not decrypted an application message from `L` sealed at the
  current epoch within the last hour (then `L` is not behind; chosen, not
  measured);
- it has not handled a re-entry for `L` at this epoch, nor accepted one for
  `L` within the last hour (chosen, not measured).

Who acts, per route:

| Route | Re-admitter | Authority today | Needs the new arm (M2) |
|---|---|---|---|
| 1 | any foreground device of the same account in the conversation | own-account Remove and Add (ADR-0050 self-service), sweepable by the account (`member_may_sweep`) | no (the account keeps a second leaf) |
| 2 | the other person's device | a plain member: may Add a device of an account present, may not Remove another's leaf; a person's last leaf is unremovable (`one_to_one_allows`) | yes |
| 3 | the acting committer (owner or admin, ADR-0045 heartbeat) | may remove any leaf except the owner's last | yes, when `L` is the owner's last leaf |

The commit is the ADR-0045 path: the re-admitter proposes `Add(K)` and then
`Remove(L)` by reference and sweeps (or its account's or the acting
committer's sweep does), through the ADR-0081 commit route and the ADR-0040
outbox. One commit replaces the leaf. If two re-admitters act at once, the
sweep's one-Add-per-signature-key clause and the DS's one-commit-per-epoch
rule leave one replacement; the loser abandons.

The Welcome goes to the person's account mailbox (as sweep Welcomes do,
C-69); for route 1 the sibling may also post it to the DeviceGroup.

### 5. Coming back: joining over the stale group

`join_from_welcome` refuses an active stored group (C-88), and must keep
doing so for every other Welcome. A new entry point, `Group::join_reentry`
(M3), replaces the stored active group only when **all** hold:

- the device holds a durable re-entry record for this group, and the
  Welcome adds exactly that record's `K` (by KeyPackageRef);
- the Welcome's group id is the stored group's;
- the Welcome's epoch is greater than the stored epoch;
- the Welcome passes the ordinary admission (role table, kind; C-86) and its
  kind equals the stored kind;
- the Welcome's sender resolves (ADR-0050 binding) to an account that is in
  the stored roster;
- the device's leaf in the new tree has the stored leaf's signature key.

Otherwise it refuses with a typed reason and changes nothing. The held-Welcome
pass changes accordingly: a Welcome refused as `GroupStillActive` is first
tried as a re-entry; while a re-entry record exists for that group it is
never dropped.

On success the client keeps the conversation record and its local history
(the ADR-0050 / ADR-0045 "a retired leaf came back" adoption path already
does this on iOS and Android), clears the suspicion, deletes the re-entry
record, and writes the gap line (UX 2.4).

### 6. Changes to `lakko-mls` (invariant #3)

- **M1, the re-entry marker.** A KeyPackage extension (private-use type
  `0xFF04`, beside `0xFF01`-`0xFF03`): `version 0x01 ‖ group_id(32) ‖
  epoch_left(8, BE)`. It is authenticated by the KeyPackage's own signature
  (the leaf key), so it needs no second signature and rides with the `Add`
  to every peer. `Identity::create_reentry_key_package(group_id, epoch)`,
  bounded fail-closed codec, fuzz target `reentry_marker_decode`. **Spike S0
  decides** whether OpenMLS 0.8.1 carries a private KeyPackage extension
  through an `Add` proposal to peers' validation and what capabilities it
  requires; if it does not, the fallback is the ADR-0081 self-removal proof
  (137 bytes) under a new domain `lakko/conversation/reentry/v1\0`, carried
  as a LeafNode extension of `K` (members only; it enters the tree).
- **M2, the re-entry replacement arm.** A `Remove(L)` proposal by any member
  is authorised when the same proposer's `Add(K)` is in the pending store
  (at receipt) or in the same commit (at the commit gate), `K` carries a
  valid marker for this group and the epoch the commit leaves, `K`'s
  signature key is `L`'s, and `K`'s account is `L`'s. For that `L` only it
  lifts `OwnerUnremovable` and `OneToOneMembershipFixed` (the account keeps a
  leaf: `K`). `owner_survives` and `one_to_one_holds` hold unchanged, since
  the role table and the one-to-one rule are keyed on accounts. The sweep
  keeps the pair together or drops both, and `member_may_sweep` includes a
  re-entry pair. Conversely an `Add` whose signature key equals a current
  leaf's and that carries no valid marker is refused at every gate (it is
  how a replayed directory KeyPackage could otherwise be used to swap a
  device out).
- **M3, `Group::join_reentry`** (section 5).
- **M4, the shared rules**, pure and on both FFI surfaces so no client
  hand-writes them: `conversation_behind_verdict` (section 1, including
  "confirmed" and "cleared") and `reentry_admission_verdict` (section 4).

Property tests (names are the contract; the bodies come with the code):

- `reentry_replacement_keeps_the_signature_key_and_the_account`
- `a_device_brought_back_reads_the_next_epoch` (three members, one misses a
  commit, requests, is replaced, decrypts the next message; its author key
  is unchanged, C-68)
- `a_reentry_marker_for_another_group_or_epoch_is_refused_at_every_gate`
- `a_key_package_colliding_with_a_current_leaf_without_a_marker_is_refused`
- `a_replayed_directory_key_package_cannot_replace_a_device`
- `the_owners_last_leaf_can_be_replaced_but_never_removed`
- `a_one_to_one_leaf_can_be_replaced_but_the_people_never_change`
- `a_remove_without_its_paired_reentry_add_is_refused_at_receipt_and_in_the_sweep`
- `a_sweep_keeps_a_reentry_pair_together_or_drops_both`
- `join_reentry_replaces_only_for_the_recorded_key_package` (proptest over
  another KeyPackage, a lower or equal epoch, another group, a sender outside
  the stored roster, another kind)
- `a_refused_join_reentry_changes_nothing`
- `behind_verdict_never_fires_with_an_own_commit_owed_or_an_undrained_read`
  (proptest) and `behind_verdict_clears_when_the_journal_is_back_at_the_local_epoch`

Fuzz targets: `lakko-mls/reentry_marker_decode`,
`lakko-contacts/conversation_reentry_decode`,
`lakko-contacts/knock_key_decode`.

### 7. Messages sent while the device was out

MLS forward secrecy means the device can never decrypt rows sealed at epochs
it does not hold. The maintainer's decision 3: the device that brings it back
sends them.

1. **Route 1:** the sibling sends a history bundle for this conversation
   covering from the device's first skipped row to the replacement, over the
   existing history-bundle machinery (sealed under the DeviceGroup history
   secret, posted to the rendezvous the request names).
2. **Routes 2 and 3:** the other person's device (one-to-one) or the owner's
   or admin's device that re-adds (group) sends the messages it holds for the
   gap, sealed for the returning device only: a history bundle under an
   exporter secret of the epoch the replacement commit creates (which only
   the members after that commit hold), posted to the account mailbox with
   the Welcome. The account was a member throughout, so nothing it could not
   have read reaches it. The gap is the request's: from the first row the
   device could not read (it states that time in the request) to the
   replacement.
   Today a history bundle is sent only when a device is added to the
   account, from `runScanFlow` (web `devices/orchestrator.ts:1038,1136`, iOS
   `Devices/PairingOrchestrator.swift:1139`, Android
   `devices/PairingOrchestrator.kt:973-1003`); neither the ADR-0081 rejoin nor
   the ADR-0050 re-admission sends one.
3. **Later:** the ADR-0064 conversation vault, once built, fills what is
   saved there.
4. What none of them holds (older than the DS retention, or not on the
   re-adder): the gap line (UX 2.4).

**As built (2026-11-19, S3 web and desktop, S4 iOS and Android): one carrier
for all three routes.** The request names no rendezvous and the mailbox has
no history kind, so items 1 and 2 above are replaced by one carrier, the same
on every client:

- **secret** = `Group.exportSecret(label "lakko/conversation-reentry/history/v1",
  context = the returning leaf's 32-byte signature key, length 32)`, exported
  at epoch `epoch_left + 1` (the epoch the replacement commit created) by the
  re-admitter and by the returning device (right after its join);
- **rendezvous id** = `SHA-256("lakko/conversation-reentry/history-rendezvous/v1"
  ‖ secret)`, no separator byte; the bundle is posted to that DS rendezvous;
- **payload** = the history bundle (`sealHistoryBundleStructured`, the
  pairing bundle's format) sealed under the secret, holding the re-admitter's
  rows of that conversation with `createdAt >= gap_from_ms` (on web, its
  `countCapped` flag is set when the cap trimmed the rows or the sender holds
  no row older than `gap_from_ms`, and a receiver whose bundle is not capped
  drops its gap line);
- the re-admitter posts only while its own group is at exactly
  `epoch_left + 1` (a later commit changes the exporter; then nothing is sent
  and the gap line stays);
- **golden vector**: a secret of 32 bytes `0x42` gives the rendezvous id
  `8b4c89a03f20c038dfe71cf88eeee780a4be0af619b852cedbc63ad0dc6eb7dd`
  (web `chat/reentry-history.test.ts`).

Only the members after the replacement hold the secret. When the replacement
lands, the re-admitter also re-sends the group state a newcomer receives on
growth (name, photo, pins, profile, push routing secret, and, when it is the
owner or an admin, the knock key), since a replacement changes no member set
and no member sees growth.

### 8. Server

No further DS, discovery or auth change (phase 1b already added `commits`
to signed reads). The DS already returns `latest_epoch` on
signed reads, keeps read access for 30 days or 64 epochs, accepts knocks with
a token, and takes one commit per epoch. The new knock kind and the new
mailbox kind are inside sealed payloads the servers do not read.

## Phase 2 S0-S2, as built (2026-11-19)

Built in `lakko-mls`, `lakko-contacts`, `lakko-ffi-uniffi`, `lakko-ffi-wasm`
and `lakko-ffi-extension`; bindings regenerated and the native libraries
rebuilt. Host-tested only: no client calls any of it, nothing was walked,
nothing is deployed, `lakko-security` has not run. The S0 result is above
(section "S0 result"). Where the build departs from sections 3 to 6:

1. **The request carries `gap_from_ms`.** Section 7 says the request states
   when the device first failed to read a row; section 3's wire had no such
   field. The envelope is `"lakko/conversation-reentry/v1\0" ‖ 0x01 ‖
   group_id(32) ‖ epoch_left(8, BE) ‖ gap_from_ms(8, BE) ‖ kp_len(2, BE) ‖ K`,
   82 to 4177 bytes (`K` 1 to 4096 bytes, the knock bound; chosen, not
   measured), exact, version checked before any length
   (`lakko-contacts::conversation_reentry`). Dispatched as
   `IncomingEnvelope::ConversationReentry`; mailbox kind
   `ConversationReentry` = `0x06` (route 2).
2. **Route 3's kind is in the associated data, not a plaintext byte.** A
   re-entry knock row has the join row's version byte, knock version and
   both sizes (4 KiB / 16 KiB); its plaintext is `len(2) ‖ envelope ‖ zeros`
   sealed with the knock labels under the AAD domain
   `lakko/group/reentry-knock-row/v1\0`. A join opener refuses it and a
   re-entry opener refuses a join row (`seal_reentry_knock` /
   `open_reentry_knock`), so the DS sees the same row either way and the join
   row's format is unchanged.
3. **The knock-key envelope** is `"lakko/group-knock-key/v1\0" ‖ 0x01 ‖
   knock_version(2, BE) ‖ knock_pk(32)`, 60 bytes
   (`lakko-contacts::knock_key`), dispatched as `IncomingEnvelope::KnockKey`;
   a receiver keeps a strictly newer version (`knock_key_is_newer`) from the
   owner or an admin resolved at receipt.
4. **M1.** `K`'s leaf advertises `0xFF04` beside the ordinary capabilities
   (S0), and `K` is a **last-resort** package: OpenMLS deletes a one-time
   package while it stages a Welcome, before any check, so a refused
   `join_reentry` would otherwise consume it. `Identity::forget_key_package`
   (new) deletes the private keys of a superseded request.
5. **M2 is one function, `reentry::judge`, at four call sites:** the
   proposer's own check (`propose_add_member`, `propose_remove_member`), the
   receipt gate, the sweep (`sweep_keeps`, the emptiness prediction, and
   `selection_is_admissible` for the exclusion search) and the peer commit
   gate. The marker must name the group's CURRENT epoch at every gate (the
   proposal store empties on every merged commit, so that is the epoch the
   commit leaves). A valid re-entry `Add` skips the per-account device limit
   (it replaces a leaf). `member_may_sweep` admits a re-entry `Remove` when
   its paired `Add` is itself in the sweeper's scope. The pair must come from
   the same sender, so the re-admitter proposes both
   (`Group::propose_reentry_replacement`, `Add` first: a `Remove` that stands
   on the pair is refused at receipt unless the `Add` is already stored).
   `Group::reentry_request_target` is the core half of section 4 (valid
   package, marker for this group and epoch, a current leaf with its
   signature key and account).
6. **M3 takes the record from the caller.** `Group::join_reentry(welcome,
   identity, recorded_key_packages)`: the core cannot see the client's
   durable record, so the client passes the wire KeyPackages it recorded.
   A recorded `K` matches when the Welcome's secrets name its
   `KeyPackageRef`, its leaf's encryption key is the new tree's leaf with
   this device's signature key, and the Welcome's epoch is its marker's
   `epoch_left + 1` (an addition to section 5's list). The stored group's
   own leaf must carry this identity's signature key. Every refusal is the
   one typed reason `ReentryWelcomeRefused` (`DoNotRetry`) with a distinct
   sentence; the M2 refusals are `ReentryPairInvalid` (`NotPermitted`).
   Both are appended to `RefusalReason`, `FfiError` and `FfiRefusalReason`.
7. **M4** is `lakko-mls::conversation_behind`: `conversation_behind_verdict`
   folds one observation into the previous state (an observation that
   neither suspects nor clears keeps it; confirmed never goes back to
   suspected; a drained read at or below the local epoch clears),
   `reentry_admission_verdict`, plus `reentry_request_due` (a request goes
   out as soon as the device is suspected, then every 30 minutes) and
   `reentry_route`, written once instead of three times. The numbers are
   section 1's and open question 5's, chosen, not measured.
8. **Tests changed by the collision rule.** Nine sweep tests used "re-add a
   current member" as their stand-in for an uncommittable proposal; that
   `Add` is now refused at authorisation, so they use a KeyPackage without
   the group's ciphersuite capability, or a re-entry package where the test
   is about a remove-and-re-add. Two test fixtures had minted a "new" device
   or newcomer with the same seed as an existing member; their seeds were
   changed.
9. **A refused join changes nothing** is pinned by comparing the identity's
   whole storage snapshot before and after each refused case.

## Abuse

- **A stranger cannot cause a replacement.** A valid `K` needs the private
  signature key of a current leaf `L`. A stranger also cannot open the
  routes: route 1 needs the account's lineage seed, route 2 a contact reach
  grant, route 3 a write to the knock queue and a payload only the knock seed
  holders open; whatever they post fails the admission checks.
- **A replayed or old KeyPackage cannot swap a device out**: an `Add` that
  collides with a current leaf's key is refused unless it carries a marker
  for this group and this exact epoch (M2), and one epoch has one commit.
- **A removed device cannot come back by this route**: route 1 checks the own
  DeviceGroup roster, routes 2 and 3 the directory listing and its
  tombstones (ADR-0074); a person removed from the group has no leaf to
  replace.
- **A member can make its own device churn**: each re-entry is a commit.
  Bounded by one accepted re-entry per leaf per hour per re-admitter and by
  refusing a re-entry for a leaf that wrote at the current epoch in the last
  hour (both chosen, not measured).
- **A member registrar can put every device into "suspected"** with a fake
  epoch (section 1). Bounded: nothing is deleted, nobody re-admits, the owner
  or an admin's repair clears it; confirmed only after 10 minutes. G-58.

## Scale

Per device and conversation, detection adds no request: `latest_epoch`
already rides every signed read, and the failed-row bookkeeping is local.
A behind device sends at most one request every 30 minutes. A replacement
is one commit regardless of group size (one Remove and one Add, logarithmic
tree work in MLS) and one Welcome. A re-admitter's check is one KeyPackage
signature and, for routes 2 and 3, one directory fetch of one account (the
fetch the ADR-0050 arm already does). Route 3 costs one `ds-knock` token per
request and one row in a knock queue capped at 4096 (ADR-0087). The in-group
knock-key envelope is one message per key version and per growth, throttled
like the group name. Nothing grows with contacts times contacts.

## Alternatives

1. **Hold the cursor on a failed commit row (do not skip).** ADR-0081 does
   this for the DeviceGroup (`FailedRowVerdict`). It keeps the signal but
   recovers nothing, and a garbage row would wedge the poll. Kept only as a
   source of the bookkeeping in section 2.
2. **Re-found or re-join through an external commit** (ADR-0064 section 4).
   Needs a GroupInfo of the current epoch, which a behind device does not
   have (the head of a conversation vault would carry one, unbuilt), and in
   a role-table group an external commit is accepted only for an account
   already present with the ExternalInit alone (ADR-0064 rule, unbuilt for
   conversations). Kept as the future path once the vault exists: it would
   let a device come back with nobody online.
3. **Remove the stale leaf and add the device as new** (a plain remove plus
   ADR-0050 re-admission, no new MLS arm). Fails exactly where it is needed:
   the owner's last leaf and a one-to-one person's last leaf are
   unremovable, and between the two commits the person would leave and
   re-enter the conversation.
4. **Rotate the device identity for that conversation** (a new leaf key, so
   the ADR-0050 arm sees "a missing device" and any member adds it). One
   device identity serves every conversation and the directory lists one key
   per device; a per-conversation identity breaks that, and the old leaf
   still needs removing.
5. **Ask the person to fix it** (a "Rejoin" button, or "remove and re-add
   me"). Rejected by the maintainer's decision (automatic) and because the
   person cannot know why the chat went quiet.
6. **Ask all members through their mailboxes** (route 3 via the owner's and
   admins' account mailboxes instead of the knock queue). Needs a `member`
   reach grant per admin; those rotate on every shrink of any of their groups
   (ADR-0078 section 9), so a device behind for a while would often hold
   stale ones. Open question 3.

## Consequences

- **Positive:** a device that missed one update no longer goes silent
  forever; the person keeps their place, role and authorship; an owner's or
  a one-to-one person's only device can come back; the JournalRepair hazard
  of a behind owner or admin is closed by the same evidence.
- **Negative:** a fourth membership arm in `authorise` and a new join path
  over an active group, both in the MLS state machine (invariant #3; the
  G-39 external review surface grows); three request routes; a new in-group
  envelope; messages sent while out reach the device only as far as the
  re-adder holds them; recovery on route 3 waits for an owner or admin, and a group whose only
  re-admitter is the stuck device itself recovers only once the owner opens
  another device there (an ADR-0050 admission by any member brings that
  device in, then route 1 applies).
- **Neutral:** nothing changes on the servers; skipping unreadable rows
  stays.

## Invariants and the index

- **#2, #5:** unchanged; routes 2 and 3 are sealed, route 1 is a DeviceGroup
  row; the DS sees one more knock row in route 3, as for a join request.
- **#3:** M1-M3 change the state machine; this ADR, the property tests above
  and fuzz targets; flag for `lakko-security`; G-39 surface grows.
- **#4:** nothing is sealed at a stale epoch once confirmed; no fallback to an
  unencrypted path.
- **C-88** amended: a Welcome also replaces an active stored group under a
  durable re-entry record that names the KeyPackage it adds (section 5).
- **C-64:** a suspected device emits no beacon.
- **C-68:** authorship survives a re-entry (same signature key).
- **New C-93 (proposed, phase 2):** a device behind in a conversation never commits,
  sweeps, registers or repairs there, and is replaced only by a commit adding
  a KeyPackage it minted for that conversation and epoch.
- **New C-92 (proposed, phase 1 and 1b):** the conversation row the DS
  names as the registered commit leaving the device's epoch is never
  skipped on a first failure: it is held and retried, and a final skip
  marks the device behind; a group loaded below the epoch recorded with its
  cursor reads that commit again; every failed apply is logged without
  identifiers.
- **New G-58 (proposed):** a member registrar can put devices into
  "suspected"; (closed in phase 1b: a commit-shaped garbage row no longer
  holds anyone, the DS names the registered commit); a
  single-device account removed while its device was behind waits with no
  end; a group whose only owner or admin is the behind
  device recovers only through another device of the owner.

## Plan

- **Phase 1 (built):** the hold rule, the backoff, the behind mark, the
  log line and the journal-repair guard on web (incl. desktop), iOS and
  Android, each with tests; no server and no `lakko-mls` change.
- **Phase 1b (built, host-tested):** `commits` on signed DS reads and the
  exact hold rule; the cursor epoch and the rewind; the DeviceGroup loops on
  the exact rule (web aligned); identifier-free log lines. No `lakko-mls`
  change.
- **S0, spike (core, scratch) (done 2026-11-19):** OpenMLS 0.8.1 behaviour
  for a private KeyPackage extension through an `Add`; a pending `Add` whose
  signature key collides with a current leaf; the pair building in one
  sweep.
- **S1, core (built 2026-11-19, host-tested):** M1-M4, property tests, fuzz
  targets; `lakko-contacts` `conversation_reentry` and the knock-key
  envelope; the new mailbox kind.
- **S2, FFI (built 2026-11-19, host-tested):** UniFFI and wasm; the NSE
  surface recognises the new envelopes to suppress them.
- **S3, web (and desktop):** detection, the stop list, routes 1-3, the
  re-admitter, `join_reentry`, the held-Welcome change, the JournalRepair
  gate, the UX.
- **S4, iOS and Android:** the same.
- **S5, live:** the stuck desktop group `c14ae3b45bdb6a66...` on the test
  stack is the first walk (it must come back by itself); then a one-to-one
  and an owner's-only-device group made to miss a commit on purpose.

## Open questions

Decided on 2026-11-18 (see "Maintainer decisions"): sending while out (hidden
composer), the time limit (none), missed messages (the re-adder sends them),
who re-adds in a group (owner or admin), and the order (prevention first).
Still open:

5. **The numbers chosen without measurement:** phase 1's backoff and its 8
   attempts; phase 2's re-request every 30 minutes, 10 minutes before a
   suspicion without other evidence is shown, the one-hour per-leaf limits,
   30 seconds before the second line of the note.
6. **Naming the registered commit exactly.** CLOSED 2026-11-18 (maintainer
   approved; built in phase 1b, section 1). As first written: Phase 1 identifies it from the
   framed epochs and `latest_epoch` (exact for honest devices). The DS could
   name it in each read (`commit_message_id` per registration, stored since
   ADR-0081), which closes the garbage-row case above. A server change; not
   made.
7. **The web DeviceGroup loop** skips a failed commit where iOS and Android
   hold it (ADR-0081). Align it? CLOSED 2026-11-18 (maintainer: align;
   built in phase 1b, section 3, with all three on the exact rule).

## References

- ADR-0045, ADR-0050, ADR-0053, ADR-0057, ADR-0062, ADR-0064, ADR-0074,
  ADR-0078, ADR-0081, ADR-0087
- RFC 9420 section 10 (KeyPackages and their extensions), section 12.1
  (proposals), section 12.4.3 (Welcome)
- `docs/ux/conversation-rejoin.md`
