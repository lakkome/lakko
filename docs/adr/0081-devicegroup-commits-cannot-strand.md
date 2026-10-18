<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0081: A DeviceGroup commit cannot strand the account

- **Status:** Accepted (2026-10-18, maintainer: as written, and the
  register-and-commit route also for ordinary groups, open question 2)
- **Date:** 2026-10-18
- **Context:** multi-device (ADR-0007, ADR-0050), journal authorisation and
  ownership (ADR-0053, ADR-0057), the commit outbox of ordinary groups
  (ADR-0040), proposal-based membership (ADR-0045, which leaves the
  DeviceGroup out), device removal (ADR-0074), servers that cannot link
  accounts (ADR-0078)
- **Amends:** ADR-0057 (an epoch is registered together with the commit
  that reaches it, for the DeviceGroup and for ordinary groups), ADR-0040
  (the outbox publishes through the register-and-commit route), ADR-0050 (how a device that lost its
  DeviceGroup state comes back)

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
longer register DeviceGroup epochs through `/access`. Open question 1
asks whether the DS should refuse a plain `/access` Append on a
DeviceGroup journal as well.

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
- No `lakko-mls` change (invariant #3 untouched).
- Invariant #2 holds: the DS binds a key to a row hash and never reads
  the row.
- Invariant #5 holds: the knock and the directory carry what they carry
  today.
- A new invariant: a DeviceGroup epoch key exists on the DS only together
  with the commit row that reaches it.

## Open questions

1. Should the DS refuse a plain `/access` Append on a DeviceGroup journal
   (forcing the `commit` route), and how would it know the journal is a
   DeviceGroup without learning anything new? One option is a flag in the
   claim statement.
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

## Documentation this ADR implies

- CLAUDE.md ADR row, plus the `services/ds` and `lakko-ds-proof` module
  rows.
- A new invariant in `docs/security/invariant-index.md` and `calls.md`.
- A known-gaps entry until it is built.
- The PRODUCT-READINESS multi-device row.
