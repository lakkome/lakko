<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0081: A DeviceGroup commit cannot strand the account

- **Status:** Accepted (2026-10-18, maintainer: as written, and the
  register-and-commit route also for ordinary groups, open question 2; the
  DS refuses a plain `/access` Append for every journal, open question 1;
  direct commits of ordinary groups are staged, not eagerly merged, and the
  PublisherGroup record keeps the epoch-N access seed, the same day; a
  rejoin under the attested-removal policy carries a self-removal proof,
  2026-10-18, see "Amendment (2026-10-19)"; a conversation is joined only
  from a Welcome carrying a role table and a kind, and the clients'
  ordinary-group direct-commit paths are deleted, 2026-10-19, see
  "Amendment (2026-10-19, second)").
  Built 2026-10-18, see "As built"; both amendments built 2026-10-19;
  host-tested only, not walked live, not deployed.
- **Date:** 2026-10-18
- **Context:** multi-device (ADR-0007, ADR-0050), journal authorisation and
  ownership (ADR-0053, ADR-0057), the commit outbox of ordinary groups
  (ADR-0040), proposal-based membership (ADR-0045, which leaves the
  DeviceGroup out), device removal (ADR-0074), servers that cannot link
  accounts (ADR-0078)
- **Amends:** ADR-0057 (an epoch is registered together with the commit
  that reaches it, for the DeviceGroup and for ordinary groups), ADR-0040
  (the outbox publishes through the register-and-commit route, and a direct
  commit is staged and recorded before it is sent), ADR-0045 (the staged
  lifecycle of the sweep also carries direct commits), ADR-0050 (how a
  device that lost its DeviceGroup state comes back), ADR-0015 (the
  removal gate also accepts a self-removal proof, see the amendment),
  ADR-0062 (a conversation Welcome needs the role table as well as the kind,
  see the second amendment), ADR-0040 / ADR-0045 (the direct-commit paths
  of ordinary groups are deleted on the clients, second amendment)

## Context

A read-only reconnaissance on 2026-10-18 asked whether two of an
account's devices can fork its DeviceGroup by committing on the same
epoch. They cannot, through any shipped path:

- On web, iOS and Android, every DeviceGroup commit path (add a device by
  pairing or by the ADR-0049 confirmation, remove a device, enable
  attested removals) creates the commit on a throwaway handle restored
  from the durable snapshot. It then registers the next epoch's access key
  on the DS (`PUT /ds/group/{hex}/access`, Append mode), persists the new
  snapshot, and publishes the commit under the old epoch's key.
- The DS accepts an Append only at `latest + 1` or as an identical
  re-send (`services/ds/src/ownership.rs` `apply_register`). A second
  device's commit derives a different key and gets `409`. Every client
  then drops its handle without persisting or publishing.

The DS registration is therefore already the arbiter of "which commit
owns epoch N+1". What is missing is the guarantee that the commit which
won the registration reaches the journal. Three paths break it:

1. **Crash between the registration and the persist** (all clients). The
   DS holds N+1, every device holds N, and the commit exists nowhere.
2. **Publish fails after the persist** (all clients). No DeviceGroup
   commit outbox exists, unlike ordinary groups (ADR-0040), so nothing
   retries. The committer sits alone at N+1.
3. **Persist failure swallowed** (web `persistDeviceGroupSnapshot`,
   Android `persistSnapshot`). The commit is published while the device
   stays durably at N, behind its own commit. iOS fails closed.

A root-registered epoch cannot be replaced (`apply_register` refuses a
Replace over an equal rank), so a dead N+1 key blocks every other
device's Append. The existing repair (the "fork verdict": the DS's
latest epoch above the local one for 120 s) then deletes the snapshot,
re-founds the DeviceGroup and resets the DS chain. It does so on **each**
other device independently, at most once per session. The account falls
apart into single-device DeviceGroups, and every device must be added
again by hand.

A device whose DeviceGroup snapshot is unreadable takes the same
re-found-and-reset path today (web `devices/lifecycle.ts`), cutting off
live siblings.

## Decision

### 1. The DS registers an epoch and appends its commit in one step

A new route `POST /ds/group/{hex}/commit` carries:

- an Append `RegisterEpoch` statement for N+1 signed by the journal root
  (ADR-0057; for the DeviceGroup every device holds the account-derived
  root);
- the request proof under an epoch-N key in the write tier (ADR-0053);
- the row bytes (the ADR-0015 DeviceGroup envelope around the commit).

In one transaction, behind the journal's lock row, the DS:

- verifies both signatures;
- accepts the Append at `latest + 1`;
- appends the row;
- records `BLAKE3(row)` with the registration.

An identical re-send (same key at `latest`, same row hash) appends
nothing and answers the original message id. Any other Append at an
existing epoch is `409` as today. The DS still never parses the commit
(invariant #2); it only binds "this key registered" to "this row
appended", so a registration without its commit row cannot exist.

All three clients use this route for every DeviceGroup commit and no
longer register DeviceGroup epochs through `/access`. Since open question 1
was decided (2026-10-18), the DS refuses a plain `/access` Append for every
journal, so this route is the only way a new epoch is registered.

### 2. A durable DeviceGroup commit outbox on every client

The ADR-0040 shape, with one change in the order. The DeviceGroup's
current snapshot stays at N until the DS has answered.

1. Build the commit on a throwaway handle, as today.
2. Write an outbox entry: commit row bytes, Welcome bytes and recipients,
   ADR-0015 attestation, the N+1 snapshot. Write it beside the current N
   snapshot, not over it. If this write fails, abort; nothing has left
   the device.
3. Send `POST .../commit`.
4. On success, make the entry's N+1 snapshot current (fail closed), then
   deliver the Welcome, then delete the entry.
5. On `409`, delete the entry; the device stays at N and applies the
   winner's commit from the journal.
6. On any other failure or a restart, the entry is retried. The re-send
   is idempotent on the DS, so a crash after the DS answered but before
   step 4 completes on the retry.

An entry is also re-sent before any new DeviceGroup commit is built, so
two commits of one device never interleave. Forget device removes the
outbox with the other per-account stores (ADR-0077 store gate).

### 3. Every DeviceGroup snapshot write fails closed

Web and Android stop swallowing DeviceGroup persist errors. A failed
write aborts the operation in progress, as on iOS.

### 4. A device that lost or fell behind its DeviceGroup rejoins

A device rejoins instead of re-founding when either holds:
- its DeviceGroup state is unreadable or lost;
- it stays behind the DS's latest epoch after the journal is drained.

Re-founding is reserved for an account with no other device. The
re-found-and-reset repair is removed.

1. The device deletes its local DeviceGroup state and publishes a fresh
   KeyPackage to the directory (ADR-0050). The KeyPackage carries its
   signed device binding and is published under its own device-bound
   session (ADR-0074).
2. It posts that KeyPackage to the DeviceGroup's knock (ADR-0053, one
   `ds-knock` token). The DeviceGroup id is derivable from the account
   seed (ADR-0078 phase 6).
3. A sibling device reading the knocks admits the KeyPackage when all of
   these hold:
   - its device binding verifies under the account key;
   - its device id is listed in the account's directory and not
     tombstoned (ADR-0074);
   - the sibling holds the DeviceGroup.

   To admit, the sibling removes the device's previous leaf if it is
   still in the roster, then adds the new one. These are ordinary
   DeviceGroup commits through the outbox of decision 2, so the DS
   arbitrates concurrent siblings. The device joins from the Welcome.
4. The device re-founds the DeviceGroup only when the account's
   directory lists no other own device. That is a single-device account,
   where nobody can be cut off.

This uses only the existing `add_device_from_wire` and `remove_device`.
It needs no external-join support in the DeviceGroup and no `lakko-mls`
change. **Amended 2026-10-19:** the removal and the add are one commit, and
under the ADR-0015 policy the knock carries the device's own self-removal
proof, a `lakko-mls` change; see "Amendment (2026-10-19)".

## Alternatives

- **Proposal-based commits with an elected committer (ADR-0045) for the
  DeviceGroup.** Unnecessary. The DS registration already orders
  DeviceGroup commits, and the DeviceGroup has no role table to elect
  from. It would bring the staged-commit lifecycle into a group that does
  not need it.
- **Let the root Replace a dead latest epoch.** The DS cannot tell a dead
  registration from one whose commit is still on its way. A Replace would
  orphan a live commit, which is the fork this design avoids.
- **Keep the re-found repair with a longer grace period.** It still
  splits the account whenever it fires; a longer wait only makes that
  rarer.
- **Client outbox alone, without the DS route.** It covers paths 2 and 3.
  It cannot cover a device that registers and is then lost for good: its
  outbox is gone with it, and the key it registered blocks every sibling
  forever.

## Consequences

- `services/ds`:
  - the `commit` route;
  - a migration adding the row hash to the registration;
  - DB-gated tests: atomicity, idempotent re-send, `409` on a different
    key.
- `lakko-ds-proof` / both FFI surfaces: a request body or statement
  helper for the new route, if the existing register statement and proof
  do not suffice as they are.
- Web, iOS, Android:
  - the DeviceGroup outbox, wired into the three commit paths (pairing
    and confirmation, removal, attested-removal policy);
  - fail-closed persist;
  - the rejoin flow (device side and sibling side);
  - removal of the re-found-and-reset repair and of the unreadable-
    snapshot founder bootstrap;
  - tests per client for each failure path above.
- No `lakko-mls` change for the DeviceGroup. The maintainer's decision of
  the same day on ordinary-group direct commits (see "As built") is a
  `lakko-mls` change under invariant #3: staged twins of the direct commits,
  with property tests; it grows the G-39 surface.
- Invariant #2 holds: the DS binds a key to a row hash and never reads
  the row.
- Invariant #5 holds: the knock and the directory carry what they carry
  today.
- A new invariant: a DeviceGroup epoch key exists on the DS only together
  with the commit row that reaches it.

## Open questions

1. **Decided 2026-10-18 (maintainer): yes, for every journal.** The DS
   refuses a plain `RegisterEpoch` in Append mode through
   `PUT /ds/group/{hex}/access` for DeviceGroup journals and ordinary groups
   alike; a new epoch is registered only together with its commit row
   through `POST .../commit`. No flag in the claim statement is needed and
   the DS learns nothing new about a journal. What stays on `/access` and
   its sibling routes: the claim (the journal's first registration), the
   ADR-0057 Replace from a higher rank, the registrar set and the root
   transfer.
2. **Decided 2026-10-18: yes, in the same work.** Ordinary groups
   register before publishing too, and a dead key registered by the owner
   (root rank) cannot be replaced by anyone. Every commit an ordinary group
   publishes (the ADR-0045 staged sweep and the remaining direct commits)
   goes through the same `commit` route, signed by whichever registrar
   (member, admin, root) registers it today, so the registration and the
   row land together. The ADR-0040 outbox already holds those commits; its
   publish step becomes the `commit` call and its re-send is idempotent on
   the DS. The ADR-0057 Replace from a higher rank stays: it is still how
   an owner or admin repairs an epoch a lower-rank registrant (for example
   an evicted member) registered with a row nobody can apply. A `409` on a
   staged sweep abandons it as a lost verdict does today.

## As built (2026-10-18)

Host-tested only. Nothing here was walked live on a device, and nothing
was deployed.

### The DS

- `POST /ds/group/{hex}/commit` (`services/ds/src/commit.rs`), body
  `{"statement_b64", "row_b64"}` (unknown fields refused), the request proof
  in `x-lakko-ds-proof`. Answers: `201 {id, created_at}` for a new row,
  `200` with the original `{id, created_at}` for the identical re-send;
  `400` for a malformed body, a statement that is not an Append
  `RegisterEpoch`, another journal, an empty row, or a proof whose epoch is
  not the commit's epoch minus one; `401` for a stranger signer or a proof
  that does not verify (the uniform refusal); `403` for a rank refusal of
  ADR-0057; `409` when the epoch is already registered with another key or
  row, or is not `latest + 1`.
- Migration `0013` adds `commit_row_hash` and `commit_message_id` to
  `ds_group_access`; the row is appended by `journal::insert_row` inside the
  same transaction. A Replace (ADR-0057) clears the binding of the epoch it
  overwrites.
- **Two places where the build reads the ADR more narrowly or more broadly.**
  The re-send is recognised before the write tier and before any nonce is
  spent, proven by any retained epoch key (the ADR said "same key at
  latest"): a device whose siblings moved on past the epoch can still learn
  that its commit landed. A conflict (`409`) is decided before the write
  tier, so a lagging device learns it lost rather than getting the uniform
  `401`.
- **Open question 1:** `PUT .../access` answers a plain Append with
  `400 {"error":"append-needs-commit"}` for every journal, checked before the
  signature (`ownership::append_needs_commit`). The claim, the Replace, the
  registrar set and the root transfer are unchanged.
- DB-gated tests in `commit.rs` and `ownership.rs`: the fields are pinned;
  key and row land together; a rolled-back transaction leaves neither (a
  revocation refused with `403` writes nothing); the identical re-send
  answers the original id (proof under N, under N+1, and after the siblings
  moved on); a commit with another key or another row, or at an epoch
  registered with another row, is `409`; each rank (member, admin, root)
  commits, a stranger and a bare epoch key are `401`; a higher-rank Replace
  still repairs; a plain Append on `/access` is refused while a claim and a
  Replace still work. The shared test helper registers epochs through the
  commit route.

### Core and FFI

- `lakko-ds-proof::commit`: `commit_body_json`, `commit_row_hash`
  (`BLAKE3(row)`), `is_commit_statement`, `StatementSigner::sign_commit_at`.
  No new parser: the DS reads the body with serde and the existing
  statement decoder (already fuzzed by `statement_decode`), so no fuzz
  target was added.
- `DsStatementSigner.commit(journalId, epoch, epochKey, revoke, row)` on
  UniFFI and wasm returns the path and the exact body. `register_epoch` /
  `registerEpoch` is removed from both surfaces, so no client can build a
  plain Append any more. Bindings regenerated; `LakkoCore.xcframework`,
  the extension framework, the sframe-native archive and the web wasm
  rebuilt.
- **`lakko-mls`: staged direct commits (maintainer, 2026-10-18; an
  invariant-#3 change).** The first build sent a direct commit before
  recording it, which reopened the G-35 brick ADR-0040 closed (a crash
  between the DS's answer and the record left the device below its own
  commit). The maintainer refused that order. `Group` now has a staged twin
  of each direct commit a client sends to a Delivery Service:
  `stage_add_members` (and `stage_add_members_from_wire`),
  `stage_remove_member`, `stage_set_group_roles`. A twin builds the same
  commit with the same local refusals and leaves the group at epoch N
  holding it as pending, exactly as `stage_pending_proposals` leaves a sweep,
  so the existing lifecycle serves both: `has_staged_sweep`,
  `staged_export_secret` (the N+1 key), `merge_staged_sweep`,
  `abandon_staged_sweep` (a per-group `clear_pending_commit`, never a
  whole-provider restore). While any commit is staged every membership
  operation, eager or staged, is refused with the typed
  `SweepAlreadyStaged` (OpenMLS refused it too, untyped). The eager paths
  stay for commits no Delivery Service decides on (a founding commit nobody
  else receives, a `PublisherGroup` rotation or approve). Recon of the
  clients: the direct commits sent to a DS are the add (one KeyPackage or
  one person's several), the remove, and the role-table rewrite in a group
  without the proposal path; ownership transfer is proposal-only, the
  external-join approve is a proposal (ADR-0045 S7), founding commits are
  claimed rather than sent, and call groups are relayed by the SFU, not the
  DS journal. Property tests in `tests/staged_direct_commit.rs` (12, one a
  proptest): a merged staged commit reaches the eager path's epoch, roster
  and role table, peers apply it, the added member joins from its Welcome,
  all share one exporter secret, and the staged N+1 key equals the merged
  epoch's; abandoning leaves the group exactly at N (epoch, roster, roles,
  exporter secret, it still sends) and a sibling group of the same identity
  untouched; a staged commit survives a storage snapshot and a restore into
  a fresh handle and can then be merged or abandoned; a second commit of
  any kind, eager or staged, and a sweep stage are refused while one is
  staged. On UniFFI (`stageAddMemberFromWire`, `stageAddMembersFromWire`,
  `stageRemoveMember`, `stageSetGroupRoles`) and wasm (`stageAddMember`,
  `stageAddMembers`, `stageRemoveMember`, `stageSetGroupRoles`); no new
  `FfiError` variant (the refusal is the existing `SweepAlreadyStaged`).
- **The PublisherGroup access seed (maintainer, same day).**
  `PublisherGroup.dsAccessSeed(identity)` returns the 32-byte access seed
  of the current epoch and `DsProofSigner.forGroup(epoch, seed)` rebuilds
  the signer from it, on UniFFI and wasm, so a PublisherGroup commit can
  record the proof of the epoch it left (below).

### The three clients

- **DeviceGroup outbox** (web `devices/device-group-commit.ts` +
  `device-group-commit-runtime.ts`, iOS `Devices/DeviceGroupCommit.swift` +
  `DeviceGroupCommitRuntime.swift`, Android `devices/DeviceGroupCommit.kt` +
  `DeviceGroupCommits.kt`): as decision 2. The entry is a pending row beside
  the snapshot (web IndexedDB store `pending`, iOS and Android a sealed file
  per kind beside the snapshot); pairing, the ADR-0049 confirmation (which
  runs the pairing commit), removal, the attested-removal policy and both
  rejoin commits go through it; Forget device deletes the pending rows with
  the snapshot, through the ADR-0077 store gate. One serialising lock per
  account spans "re-send what is owed, build, run". Step 4 makes the N+1
  snapshot current only while the durable state is still below the entry's
  epoch: a retry after the Welcome failed must not put back an N+1 the sync
  runtime has since moved past (two messages under one key and nonce).
  The sync runtime finishes an owed entry at every pass and, until it has,
  never reads itself as behind. On Android the re-send in the poll loop and
  the knock admissions run non-cancellable, because a landed entry reopens
  the sync runtime, which cancels its poll loop.
- **Decision 3, plus one guard the ADR did not name.** Every DeviceGroup
  snapshot write fails closed. Beyond that, each write may name the
  generation of the snapshot it read and is refused when another handle
  wrote since (web generation in the store, iOS and Android a per-process
  counter). Without it the sync runtime's long-lived handle, still at N,
  would put N back over the N+1 a commit had just made current through its
  own handle. A refused write reloads the handle. Android's sync writes now
  persist before they publish, as iOS's did.
- **Ordinary groups, direct commits (maintainer, 2026-10-18): staged,
  ADR-0040's order.** The commit is staged (the group stays at N); the
  ADR-0040 entry (commit, Welcome and its address) and the snapshot that
  carries the staged commit are written fail-closed (a failure abandons and
  sends nothing); then the commit route is called with the staged N+1 key,
  proven under N. On `201` / `200` the staged commit is merged and the merge
  persisted fail-closed (a failed write leaves the durable N + staged
  commit + entry, which the re-send turns into `200` and a merge), then the
  Welcome is delivered (marked owed first, so a failed delivery is retried
  by the owed-Welcome path) and the entry acked. On `409` or a definitive
  refusal (`400` / `401` / `403`) the staged commit is abandoned, the
  abandon persisted and the entry dropped. Anything else (the network)
  leaves the entry and the staged commit: the entry carries the sweep's
  verdict-pending flag, so the owed-verdict path that already finishes a
  staged sweep (`resolveOwedVerdict`) re-sends the same row and merges or
  abandons. The "3 tries, then reset to N" path is removed, and with it the
  reset to the durable snapshot on these paths (kept only as the fallback
  when an abandon itself fails). Where the owed verdict runs: web and iOS
  at the tail of every poll (`sweepIfCommitter`, which now also runs it for
  a group with no role table) and at the restart sweep; Android at the
  restart sweep (chat open) and after every membership action, as for its
  staged sweep (Android has no poll-tail sweep). The definitive-refusal
  arm abandons rather than waiting, which the maintainer's instruction did
  not name: such a refusal does not change on its own, and a staged commit
  waiting on it would freeze the group's membership indefinitely.
- **Ordinary groups, the ADR-0045 staged sweep:** the entry is recorded,
  then the commit route is called with the staged epoch's key; a `409`
  abandons like a lost verdict, a refusal (`400` / `401` / `403`) abandons
  and drops the entry, a network failure leaves the verdict owed and the
  owed verdict re-sends the same row. On the restart path the revocation
  list is empty (the evicted accounts are not recorded with the entry).
- **PublisherGroup (C-82):** the journal step is one commit-route call. The
  commit still merges eagerly (no Delivery Service decides on a rotation's
  local effect: the subscribers are resealed regardless), but the record now
  keeps the epoch it left and that epoch's access seed (`proofEpoch`,
  `proofSeedB64`, inside the device-key-encrypted record, gone with it), so
  the step is proven under N live and on a replay after a restart alike: a
  replay always learns the answer (appended, the original row, or `409`).
  A `409` or a definitive refusal marks the journal step lost
  (`journalLost`): an approve then delivers no Welcome and appoints nobody
  (a Welcome for a commit nobody has would put the joiner in a group nobody
  else is in) but still reseals, since the owner's posts are sealed under
  the merged seed; a lost approve leaves the owner's PublisherGroup ahead of
  its journal (the eager merge cannot be undone), which is not new and is
  not repaired here. A rotation whose journal step got no answer no longer
  drops it: the reseal runs and the entry is kept for the journal step,
  which the next replay sends again.
- **Decision 4, how "lost" and "behind" are decided.**
  - *Lost:* no snapshot, or one that does not decrypt or does not restore
    (it is deleted first; a rejoin's pending knock is kept), at the sync
    runtime's start. The runtime then reads the account's directory: when it
    lists no device other than this one, it founds; otherwise it rejoins. A
    directory that cannot be read founds nothing and is asked again. Every
    other caller of the lifecycle (pairing, removal, the policy, the device
    list) never founds; a key reset founds (the new seed names a new
    journal).
  - *Behind:* a signed read after the cursor returned no rows and nothing
    more, the DS's `latest_epoch` is above the handle's epoch, and this
    device has no DeviceGroup commit owed. Immediate: since decision 1 a
    registered epoch always has its row, so no grace period remains for a
    commit "on its way". The device must also read as active (a removed
    device does not rejoin).
  - *The device's side:* drop the snapshot, the owed commit and the
    admission record (the knock state stays); publish the directory
    KeyPackage; build a DeviceGroup KeyPackage from the same device identity
    and post it to the DeviceGroup knock as the ADR-0056 pairing payload
    (login handle, KeyPackage, a fresh rendezvous), keeping the joiner's
    provider and the rendezvous beside the snapshot before the knock goes
    out; wait for the Welcome at the rendezvous; knock again after 30
    minutes without one. Reusing the pairing payload means no new parser.
  - *A sibling's side:* once its journal is read to the head (at most every
    30 s) it reads the knocks and admits one only when the payload names the
    account's login handle; the KeyPackage's leaf key is listed in the
    directory with a device binding under the account key the conversation
    leaves are bound to (an unbound leaf is not a binding; a deleted device
    is tombstoned and not listed, ADR-0074); the knock is newer than this
    device's last sighting of that key being added and than when it started
    watching (a durable admission record, at most 256 handled knock ids);
    and this device is the lowest leaf other than the knocking key, or the
    knock has waited 10 minutes. It then removes the key's previous leaf if
    present and adds the KeyPackage, two commits through the outbox; the DS
    arbitrates two siblings. The constants are chosen, not measured.
  - *Single-device account:* founds and claims as before.
- ~~**Not built:** under the ADR-0015 attested-removal policy, a knock whose
  previous leaf is still in the roster waits for a removal the person
  authorises with a passkey.~~ Closed by the amendment of 2026-10-19 (the
  self-removal proof). The re-found repair and the unreadable-snapshot
  founder bootstrap are removed on all three clients.

### Verified

Run on 2026-10-18 in the build worktree after the staged-direct-commit
change, results as printed: `cargo test -p lakko-mls` 385 passed (lib 238
plus 12 integration binaries, `staged_direct_commit` 12 of them); `cargo test
-p lakko-ds-proof` 45 passed; `cargo test -p lakko-svc-ds` against a local
MariaDB 84 passed; `lakko-ffi-uniffi` 391, `lakko-ffi-wasm` 278,
`lakko-ffi-extension` 89 passed; `cargo test -p lakko-log-hygiene
--all-features` 17 passed; clippy `-D warnings` on `lakko-mls`,
`lakko-svc-ds`, `lakko-ds-proof` and the three FFI crates clean; `cargo fmt
--all --check` clean; `audit-index-check` clean. Web: wasm rebuilt, `tsc
--noEmit` clean, vitest 466 files, 6162 tests passed. iOS:
`LakkoCore.xcframework` and the extension framework rebuilt, `xcodebuild
build-for-testing` succeeded (the app and the test target compile; the tests
were not run). Android: native libraries rebuilt, `:app:compileDebugKotlin`
green, `:app:testDebugUnitTest` 4362 tests, 0 failures, 1 skipped (counted
from the XML reports; the host JVM loads no native core, so the staged
direct-commit path of `ChatRepository` is not exercised there). Nothing was
walked live and nothing was deployed.

## Amendment (2026-10-19): a rejoin under the attested-removal policy carries its own proof

Decided by the maintainer on 2026-10-18; closes G-52. Built 2026-10-19 in
`lakko-mls`, `schemas/wire/device_pairing_v1.proto`, both FFI surfaces and
web / iOS / Android; host-tested only, not walked live, not deployed. A
`lakko-mls` change under invariant #3 (a new codec, a new commit and a
widened apply-path gate, with property tests and a fuzz target), so the G-39
surface grows. Indexed as C-85.

**The gap.** Under the ADR-0015 policy every removal from a DeviceGroup
needs a passkey attestation (`process_incoming_verifying_removals` refuses
to merge one without it). Decision 4 readmits a lost device by removing its
previous leaf and adding its new KeyPackage, and a sibling cannot obtain an
attestation without the person, so an account with a passkey waited for a
person to remove the old leaf by hand, and no flow asked for it.

**Decision.** The gate also accepts a removal covered by a **self-removal
proof**: an Ed25519 signature by the removed leaf's own signature key,
naming the epoch the replacing commit leaves and the KeyPackage that
replaces the leaf, honoured only when the commit leaves that epoch and adds
that KeyPackage.

- *The proof* (`crates/lakko-mls/src/self_removal.rs`): 137 bytes,
  `version (0x01) ‖ epoch (8, big-endian) ‖ removed_signature_pubkey (32) ‖
  key_package_ref (32) ‖ signature (64)`. The signature covers
  `"lakko/devicegroup/self-removal/v1\0" ‖ group_id_len (1) ‖ group_id ‖
  epoch ‖ removed_signature_pubkey ‖ key_package_ref`, where
  `key_package_ref` is the RFC 9420 `KeyPackageRef` of the replacing
  KeyPackage. The group id is bound but not carried.
  `SelfRemovalProof::decode` is exact-length and version-checked and fails
  closed; fuzz `self_removal_proof_decode`.
- *Why the epoch (beyond the brief).* The brief named the group, the leaf
  key and the KeyPackage. That proof is replayable: the replacement leaf
  keeps the removed leaf's signature key (the same device identity), so any
  member holding the old proof could later commit "remove the rejoined
  device's leaf, add the same KeyPackage again", a removal of another device
  without an attestation, which is exactly what the policy forbids. OpenMLS
  builds that commit (`a_used_proof_cannot_remove_the_rejoined_device_later`
  builds it). With the epoch in the statement the proof covers one commit
  only, since an epoch has one commit (C-80). The device learns the epoch
  with a knock probe (an empty knock, which the DS answers with the latest
  registered epoch and stores nothing; it spends one `ds-knock` token). Cost:
  when another DeviceGroup commit lands between the knock and the
  admission, the proof is stale and the knock waits for the next re-knock
  (30 minutes) under the policy; without the policy the sibling replaces the
  leaf without a proof as before. An unclaimed journal names no epoch, and
  the device knocks without a proof.
- *The gate.* A removed leaf is covered when an ADR-0015 attestation covers
  it, or when a self-removal proof names this group, the epoch the commit
  leaves, that leaf's key, and a KeyPackage the same commit adds
  (`Group::process_incoming_gated` now hands the gate the commit's added
  KeyPackage refs beside the removed leaves). A proof alone (a plain
  removal), a proof for another epoch, leaf, group or KeyPackage, and a
  proof signed by any other key cover nothing. With the policy off nothing
  changes.
- *Where it travels.* In the DeviceGroup journal envelope's existing
  removal-authorisation slot, beside or instead of an attestation: an
  attestation is 121 bytes under its own domain and a proof is 137 bytes
  under another, and the gate tries each entry as either, so neither can
  stand in for the other. No envelope version bump and no change to
  `processIncomingVerifyingRemovals` on any FFI surface.
- *One commit.* `Group::replace_member` / `DeviceGroup::replace_device_at`
  remove a leaf and add a KeyPackage in ONE commit with a Welcome
  (`replaceDeviceAt` on both FFI surfaces). It refuses its own leaf, an
  unknown leaf, a pending proposal or a staged commit, and a KeyPackage
  without a device binding; like the other DeviceGroup commits it merges in
  memory and goes through the DeviceGroup commit outbox (kind
  `rejoin-replace`), so the durable state stays at N until the DS took it.
- *The knock.* The rejoining device probes the knock for the latest epoch,
  then signs the proof with its device identity, whose key is its old
  DeviceGroup leaf key, over that epoch and the KeyPackage it knocks with
  (`signDeviceGroupSelfRemoval`). The knock is the ADR-0056 pairing payload
  with a new field `self_removal_proof = 16` (`buildRejoinKnock` /
  `encodeRejoinKnock`); `decode` accepts it empty or exactly 137 bytes and
  does not verify it. Fields 6 to 15 stay reserved.
- *The sibling.* It checks the proof before building anything
  (`deviceGroupSelfRemovalCovers`: the proof covers this group, its current
  epoch, the knocking key and this KeyPackage), and again inside the
  outbox's build at the epoch the commit will leave, because a commit the DS takes and every peer
  refuses would leave the sibling ahead of them. The admission verdict reads
  two more inputs: the proof covers (then a present previous leaf no longer
  waits under the policy), and a leaf already carries this KeyPackage's leaf
  node (`containsLeafOfKeyPackage`, then the knock was answered: a
  replacement keeps the key and often the leaf index, so a roster of keys
  cannot tell). It then builds one commit: `replaceDeviceAt` with the proof
  in the envelope when the previous leaf is present, `addDeviceFromWire`
  when it is not. The separate removal commit of decision 4 is no longer
  made (the `rejoin-remove` kind is still read by an outbox that holds one).
- *A device that no longer holds its old leaf key* (a reinstall, a wiped
  device) has a new device identity. Its KeyPackage key matches no leaf, so
  the sibling only adds it, and the old leaf stays until a person removes it
  with the passkey flow of ADR-0015. This is the intended behaviour: the
  person decides about a leaf whose key nobody holds.

**What the proof can and cannot authorise.** It lets the holder of a leaf's
key replace that one leaf by a KeyPackage it chose, in one commit at one
epoch. A device holding its leaf key is a member and can
already commit adds, so this gives it nothing it lacked; it cannot remove
any other device, and a used or stale proof covers nothing (the epoch). A
thief holding the old device's key could sign a proof for its own
KeyPackage, but the sibling admits a knock only after the directory listing,
binding and tombstone checks of decision 4, and a device the person removed
is tombstoned.

**Tests.** `crates/lakko-mls/src/device_group.rs`: a valid proof admits the
replacement only together with the named KeyPackage's add (and the rejoiner
joins from the Welcome with the same exporter secret); a proof never covers
a plain removal; a proof replayed with another KeyPackage, a proof made at
another epoch, a used proof replayed later with the same KeyPackage (the
commit builds and every peer refuses it), a proof for another leaf, one
signed by another device's key and one for another DeviceGroup are
refused; a replacement without any proof is refused under
the policy; an attestation still authorises a replacement; the policy-off
path is unchanged; `replace_device_at` refuses its own and an unknown leaf;
a proptest that a tampered proof covers nothing. Codec and signing tests in
`self_removal.rs`; the pairing round trip and the wrong-length refusal in
`pairing.rs`; the admission verdict in each client's rejoin suite.

**Verified (2026-10-19, in the build worktree, results as printed).**
`cargo test -p lakko-mls` 403 passed (lib 256 plus 13 integration
binaries); `lakko-proto` 6, `lakko-ds-proof` 45, `lakko-svc-ds` against a
local MariaDB 84, `lakko-ffi-uniffi` 391, `lakko-ffi-wasm` 278,
`lakko-ffi-extension` 89, `lakko-log-hygiene --all-features` 17 passed;
clippy `-D warnings` on those crates clean; the `lakko-mls` fuzz crate
checks; `audit-index-check` clean. `LakkoCore.xcframework`, the extension
framework and the Android native libraries rebuilt; web wasm rebuilt,
`tsc --noEmit` clean, vitest 467 files, 6168 tests passed; Android
`:app:compileDebugKotlin` green and `:app:testDebugUnitTest` 4364 tests, 0
failures, 1 skipped (from the XML reports); iOS `xcodebuild
build-for-testing` succeeded (tests not run). The fuzz target was compiled,
not run. Nothing was walked live and nothing was deployed.

## Amendment (2026-10-19, second): a conversation is joined only from a Welcome with a role table and a kind, and the clients' direct-commit paths are gone

Decided by the maintainer on 2026-10-19 (option (b): close the core gap
first, then delete). Built the same day in `lakko-mls`, both FFI surfaces
and web / iOS / Android; host-tested only, not walked live, not deployed. A
`lakko-mls` change under invariant #3 (a join condition), with property
tests, so the G-39 surface grows. Indexed as C-86.

**The gap.** The staged twins above served the ordinary-group direct commits
of a group with no role table. A check of whether such a group could still
reach a client found that it could: `Group::join_from_welcome` accepted a
Welcome whose GroupContext carried no `0xFF02` role table (the kind check of
ADR-0062 ran only when the table was there), and no client checked for a
role table after joining. Deleting the direct paths before closing that would
have left a joined table-less group with no way to change its membership.

**Decision, core.** `Group::join_from_welcome` admits a Welcome as a
conversation only when its GroupContext carries the `0xFF02` role table AND a
readable `0xFF03` kind. A Welcome without the role table is refused with the
typed `RefusalReason::NoRoleTable` (on UniFFI the existing
`FfiError::MembershipNoRoleTable`, on wasm `refusalReason: "noRoleTable"`;
no new `FfiError` variant); a Welcome with the table and no readable kind is
refused as before (ADR-0062). The checks run on OpenMLS's staged welcome,
before `into_group`, which is the call that writes the group state and
consumes the KeyPackage, so **a refused join leaves nothing in the
provider** (no group, and the KeyPackage stays usable). The ADR-0050 binding
check moved onto the staged welcome for the same reason.

The two table-less groups get their own entry points, and an ordinary-group
Welcome is refused by both:

- `PublisherGroup::join_from_welcome(identity, welcome, group_id,
  channel_id)` (UniFFI `publisherGroupJoinFromWelcome`, wasm
  `PublisherGroup.joinFromWelcome`): the co-publisher join of ADR-0028 OQ3.
  It also refuses a Welcome for another group than the id the link conveyed,
  before anything is written (Android's co-publisher join checked no group id
  before this).
- `DeviceGroup::join_from_welcome` (unchanged signature) and
  `DeviceGroup::join_welcome_before_account(welcome, identity)` (UniFFI
  `deviceGroupJoinWelcomeByName`, wasm `DeviceGroup.joinWelcomeByName`): the
  by-name pairing join of ADR-0056, which receives the Welcome before it
  knows the account id. Both web and the natives previously used the
  ordinary `join_from_welcome` for it.

The NSE surface (`lakko-ffi-extension`) exposes no join at all, and still
does not.

**Can the general mailbox intake join a PublisherGroup Welcome as a chat?**
Before this change it could not in practice, for a reason that was not a
guarantee: the intake skips `welcome-rendezvous` rows on all three clients
(web `contacts/client.ts`, iOS `ContactsClient.swift`, Android
`ContactsClient.kt`), and the co-publisher Welcome rides a rendezvous row; on
iOS and Android it is also opened under a separate channel identity. Nothing
in the core stopped a PublisherGroup Welcome delivered in an ordinary
`Welcome` mailbox row from joining as a chat. Now the core refuses it: a
PublisherGroup carries no role table, so `join_from_welcome` returns
`NoRoleTable` whatever row it came in.

**Decision, clients.** The direct add / remove / role-change paths for a
group with no role table are deleted on all three clients, with their staged
send-and-verdict plumbing and the C-61 seam that served only direct removals:
web (`contacts/client.ts`: `addMemberToGroupLocked`,
`removeMemberFromGroupLocked`, the direct role-change tail,
`commitStagedDirect`, `abandonStagedOrReset`, `resetGroupAfterPersistAbort`,
the `onCommitPersistAbort` option; `push/group-routing.ts`:
`publishRemoveThenRotate`), iOS (`ChatRepository.swift` direct paths and
`directCommitPathForTesting`, `DsProof.swift` `DirectCommitSteps`,
`PushGroupRouting.publishRemoveThenRotate`, the table-less owed-verdict
branch of `sweepIfCommitter`, the ADR-0040 F1 `resetGroupToDurableSnapshot`)
and Android (`ChatRepository.kt` direct tails, `commitStagedDirect` /
`abandonStagedDirect`, `rotationStepOrNull`, `onCommitPersistAbort` /
`resetGroupToDurableSnapshotOrThrow`, `CommitOutbox.kt` `withPrePersistReset`,
`runCrashSafeCommit` and the `onPersistAbort` hook of
`prepareCrashSafeCommit`, `StagedDirectCommit.kt`,
`GroupPushRouting.publishRemoveThenRotate`). The F1 in-memory reset existed
for an eager merge before the record; the staged sweep has none, and is
rolled back per group by `abandonStagedSweep`. In their place every membership
operation refuses a group with no role table with the core's typed refusal,
fail closed (web `noRoleTableRefusal` / `requireRoleTable`, iOS
`requireRoleTable`, Android `noRoleTableRefusal` / `roleTablePresent`), one
test per client. Kept: the ADR-0045 proposal and sweep path, PublisherGroup
commits, the DeviceGroup outbox, and the shared runners still used
(Android `prepareCrashSafeCommit`, used by the sweep). The core
keeps the staged twins and their tests; their docs, and those of the FFI
methods, now say that no client calls them. `MembershipOutcome`'s
`committed` arm is kept on all three clients, documented as returned by no
membership call, because its consumers share it with the sweep's result.

**C-61 after the deletion.** The rotation of the push routing secret after a
removal now starts only where a removal lands: the sweep's tail (the device
that committed it) and the poll's membership-shrink diff (every other
member). Both fire only after the roster shrank, so the rotation is sealed
after the Remove commit by construction. The seam tests and their
inverted-order companions went with `publishRemoveThenRotate`, because there
is no longer a removal call site that a refactor could reorder against the
rotation; the shrink entry point keeps its own tests on each client.

**Tests changed.** Core: `a_welcome_without_a_role_table_is_refused_and_leaves_nothing`,
`a_conversation_welcome_without_a_kind_leaves_nothing`,
`a_chat_welcome_joins_and_the_table_less_entry_point_refuses_it_leaving_nothing`,
`a_table_less_welcome_for_another_group_is_refused_before_anything_is_written`
and the proptest `welcome_admission_follows_the_table` (group.rs);
`a_device_group_welcome_joins_only_through_its_own_entry_point` and
`a_join_by_name_takes_a_device_group_welcome_and_refuses_a_chat`
(device_group.rs); `a_publisher_group_welcome_joins_only_through_its_own_entry_point`
(publisher_group.rs). Call-group Welcomes (ADR-0032, `Group::create` with a
role table and the kind) are covered by the `lakko-calls` link-invite tests.
Client tests that exercised only the deleted direct paths were deleted (web
`ds-access-commit.test.ts` and the direct cases of
`commit-outbox-reorder.test.ts`, `commit-lock.test.ts`,
`send-epoch-straddle.test.ts`, `proposal-membership.test.ts`; iOS
`StagedDirectCommitTests.swift`, the `DirectCommitSteps` and C-61 seam cases;
Android `StagedDirectCommitTest.kt`, the direct and C-61 seam cases, the
F1 reset cases of `CommitOutboxTest.kt`, whose fail-closed cases now drive
`prepareCrashSafeCommit`); tests
that only used a direct removal to park a membership call (web
`poll-lock.test.ts`, `add-path-filter.test.ts`) now park a proposal.

**Verified (2026-10-19, in the build worktree, results as printed).**
`cargo test -p lakko-mls` 411 passed (lib 264 plus 13 integration
binaries); `lakko-calls` 227, `lakko-channels` 121, `lakko-svc-ds`
against a local MariaDB 84, `lakko-ffi-uniffi` 391, `lakko-ffi-wasm` 278,
`lakko-ffi-extension` 89 passed; clippy `-D warnings` on those crates
clean; `cargo fmt --all --check` clean; the `lakko-mls` fuzz crate
checks; `audit-index-check` clean (85 C). `LakkoCore.xcframework`, the
extension framework and the Android native libraries rebuilt; web wasm
rebuilt, `tsc --noEmit` clean, vitest 466 files, 6145 tests passed; Android
`:app:compileDebugKotlin` green and `:app:testDebugUnitTest` 4347 tests, 0
failures, 1 skipped (from the XML reports); iOS `xcodebuild
build-for-testing` succeeded (tests not run). Two existing `lakko-mls`
integration tests joined a table-less group through `join_from_welcome` and
now join through `DeviceGroup::join_welcome_before_account`; one lib test
(`a_table_less_group_refuses_the_proposal_approve_and_keeps_the_direct_one`)
now joins through the crate's table-less entry point. Nothing was walked live and nothing was deployed.

## Amendment (2026-11-11, third): a restored device does not wait for a device that is not there

**Decided by the maintainer 2026-11-11.** Found walking a restore on the
test stack (web): an account restored on a new device with a passkey, its
only other device gone (lost, or its browser data cleared), never started.
Decision 4 makes a device with no DeviceGroup state rejoin "while the
account's directory lists another own device", and the gone device's
directory row stays until someone removes it, so the new device knocked
every 30 s for good. Nothing synced ran in the meantime: the username, pins
and folders the backup had restored never reached the screen ("?",
"Choose a username first"). The rule was written to keep two live devices
from splitting the account; it did not consider that the listed devices may
all be gone, which is the common case of a restore.

The maintainer's rule: the new device asks the devices in the list; a device
that answers at once (within ordinary network delay) is online; if none
answers, the new device founds the DeviceGroup itself and works.

1. **Every running device listens for knocks.** A device holding the
   DeviceGroup keeps a long poll on its DeviceGroup knocks while it is in
   the foreground (`GET .../knocks?wait=`; the DS already wakes a waiting
   read when a knock is posted), in place of reading them every
   `KNOCK_CHECK_INTERVAL_MS`. A device in the background, or not running,
   does not answer and counts as offline.
2. **A running device answers at once.** On a knock whose payload names this
   account's login handle it first posts a presence note to the knock's
   rendezvous (`lakko_contacts::rejoin_present`, a fixed 25-byte
   `"lakko/rejoin-present/v1\0" || 0x01`, no fields), before any directory
   lookup, so the answer costs one round trip; then it runs the admission
   checks of decision 4 and admits. The designated-admitter rule and
   `FALLBACK_ADMIT_AFTER_MS` are removed: every running device that passes
   the checks admits at once, and the DS commit route (decision 1) lets
   exactly one commit take the epoch; the others get `409` and drop theirs.
3. **The new device waits only as long as an answer takes.** It opens its
   long poll on the rendezvous before posting the knock, measures the knock
   request's own round trip `rtt`, and waits for a presence note or a
   Welcome for `max(4 x rtt, 1 s)` (chosen, not measured: the answer is a
   DS wake, one read and one write on the sibling, and the new device's
   read; every client logs the time the answer actually took). A presence
   note means a device is online: it then waits for the Welcome with no
   deadline, as before (re-knocking after `REKNOCK_AFTER_MS`).
4. **No answer: found.** With no presence note and no Welcome in that time,
   the new device founds the DeviceGroup (the founding path a device with no
   listed sibling already takes; the journal is claimed under the same root,
   so it is a root reset that drops the old epoch chain) and starts at once;
   the backup's synced state is then live.
5. **A device that comes back joins the new group.** A device whose
   DeviceGroup journal was reset under it (its signed read refused, or the
   DS naming an epoch chain it does not hold) is lost in the sense of
   decision 4 and rejoins by knocking; the founder, now running, answers and
   admits it. The account does not split: there is one DeviceGroup, and the
   returning device's chats keep their leaves (ADR-0050).

Costs, stated. A device that was merely asleep or in the background at the
moment of the restore is cut off from the DeviceGroup until it next runs,
and then rejoins as in point 5; the maintainer accepts this. The presence
note is not authenticated: a malicious DS can forge one (the new device then
waits for a Welcome that never comes, which it could also cause by dropping
the knock) or withhold every answer (the new device founds, which it could
also cause by refusing the old devices' reads); neither reaches message
keys.

## Documentation this ADR implies

- CLAUDE.md ADR row, plus the `services/ds` and `lakko-ds-proof` module
  rows.
- A new invariant in `docs/security/invariant-index.md` and `calls.md`.
- A known-gaps entry until it is built.
- The PRODUCT-READINESS multi-device row.
