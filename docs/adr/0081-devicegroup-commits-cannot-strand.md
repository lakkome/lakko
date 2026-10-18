<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0081: A DeviceGroup commit cannot strand the account

- **Status:** Accepted (2026-10-18, maintainer: as written, and the
  register-and-commit route also for ordinary groups, open question 2; the
  DS refuses a plain `/access` Append for every journal, open question 1;
  direct commits of ordinary groups are staged, not eagerly merged, and the
  PublisherGroup record keeps the epoch-N access seed, the same day).
  Built 2026-10-18, see "As built"; host-tested only, not walked live, not
  deployed.
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
  device that lost its DeviceGroup state comes back)

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
change.

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
- **Not built:** under the ADR-0015 attested-removal policy, a knock whose
  previous leaf is still in the roster waits for a removal the person
  authorises with a passkey; no flow asks the person for it, so such an
  account rejoins only after that leaf was removed by hand. The re-found
  repair and the unreadable-snapshot founder bootstrap are removed on all
  three clients.

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

## Documentation this ADR implies

- CLAUDE.md ADR row, plus the `services/ds` and `lakko-ds-proof` module
  rows.
- A new invariant in `docs/security/invariant-index.md` and `calls.md`.
- A known-gaps entry until it is built.
- The PRODUCT-READINESS multi-device row.
