<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0057: A journal has an owner; only those who change a group register its epochs

- **Status:** Accepted (2026-09-28, the maintainer's call: option (a) of the
  security pass)
- **Date:** 2026-09-28
- **Context:** Phase 3 (product hardening); the HIGH finding of the
  2026-09-28 `lakko-security` pass on [ADR-0053](0053-ds-journal-authorisation.md)
- **Amends:** ADR-0053 (who may register an epoch key, how a journal is claimed
  and re-claimed), [ADR-0054](0054-anti-abuse-without-client-work.md) decision 1
  (which journal may wake which push routing id)

## Context

ADR-0053 opens a claimed journal only to requests signed with a registered
per-epoch key. Any holder of a key for one of the two newest epochs may
register `latest + 1`, and the second-newest may replace `latest`. The server
cannot tell a real MLS epoch key from an invented one, so the security pass
found:

1. **An evicted member keeps the journal.** Holding epoch N, it registers N+2,
   N+3 ... with keys only it holds; the real members' N+1 falls out of the
   write tier, their registrations are refused, and the evictee keeps reading
   and writing while everyone else is locked out.
2. **A member can block its own removal** by pre-registering N+1 (and N+2)
   before the sweep; the committer's register-before-publish fails.
3. **A re-founded group with a deterministic id can never re-claim its
   journal.** A DeviceGroup (id from the account id) or a PublisherGroup (id
   from the channel id) re-created after a lost-state recovery finds the
   journal held by the old group's keys; multi-device sync or the channel's
   co-publishing dies silently.
4. **First-claim squatting on a known id** (a channel id is known to
   subscribers).

Separately, ADR-0054 S1 forwards a wake for whatever `push_gid` a publish
names, so anyone who claims a free journal can wake any group whose `push_gid`
it knows (Apple and Google see it in clear; former members keep it), including
forged call rings.

## Decision

**Every group journal has a root key held by the group's owner. Epoch keys are
registered only by registrars the root appoints, in three ranks - root, admin,
member - and a higher rank can replace what a lower one registered and revoke
it. A push routing id is bound to one journal, by the root or an admin.**

### 1. Keys

- **Root key.** Ed25519, derived deterministically on every device of the
  owner's account: `root_seed = BLAKE3-derive-key("lakko/ds/journal-root/v1",
  account_identity_seed || group_id)`. Nothing to store; unlinkable across
  groups (a server holding two roots cannot tell they share an owner).
  DeviceGroup: the owner is the account. PublisherGroup: the channel owner.
  Ordinary group: the ADR-0022 owner.
- **Registrar keys.** Ed25519. The root derives one per appointed member,
  `registrar_seed = BLAKE3-derive-key("lakko/ds/journal-registrar/v1",
  root_seed || member_account_key)`, registers its public key with a rank, and
  sends the seed to that member inside the group (an MLS application message,
  so only that epoch's members can read it; the envelope is a new bounded
  `lakko-contacts` kind addressed to one member's account key). No round trip
  from the member is needed.
- **Ranks.** `admin` for the owner's admins (they sweep membership under
  ADR-0045), `member` for every other member that may commit self-service
  changes (ADR-0050: adding or removing a device of its own account). The root
  itself outranks both.

### 2. Server rules (`services/ds`)

- **Claim.** The first claim of a journal carries the root public key and the
  epoch key, signed by the root. A claim of an already claimed journal is a
  **root reset** when signed by the same root: it replaces the whole epoch
  chain (and registrar set, when the body carries one). A claim signed by any
  other root is refused. This is what re-founds a DeviceGroup or a
  PublisherGroup after a lost-state recovery.
- **Register epoch.** `PUT .../access` for epoch `e` is signed by a registrar
  (or the root), not by an epoch key:
  - `e == latest + 1`: any registrar;
  - `e == latest`, replacing: allowed when the signer's rank is strictly higher
    than the rank of whoever registered `latest`, or it is the root;
  - anything else: root only.
  The request may carry a list of registrar public keys to revoke; a signer may
  revoke only strictly lower ranks. Epoch registration and revocation are one
  transaction, so a removal commit's registration drops the evicted member's
  registrar key in the same step.
- **Registrar management.** Add / remove registrars and change ranks: root
  only (signed). **Root transfer**: signed by the current root, naming the new
  root public key (ADR-0045 ownership transfer drives it).
- **Readers and writers are unchanged:** the two newest epoch keys write, the
  retained tail reads, knocks stay on the write tier (the security-pass fixes).
- **Push binding.** `PUT .../push-binding {push_gid}` signed by the root or an
  admin registrar binds the journal to one `push_gid` (replacing its previous
  binding). The DS stores `binding_key = keyed-BLAKE3(ds_pepper, push_gid)` with
  a unique constraint, so one `push_gid` belongs to at most one journal. A wake
  header whose `push_gid` does not match the publishing journal's binding is
  dropped (the publish still succeeds). An ADR-0043 rotation re-binds the new
  `push_gid`; the old binding is freed.

### 3. What the attacks become

- An evicted **member**: its registrar key is revoked by the removal's own
  registration; a fake `latest + 1` it raced in is replaced by the admin or
  root committer (higher rank). It keeps only the read tail, as ADR-0053 says.
- An evicted **admin** removed by the owner: the root replaces and revokes. An
  admin removed by another admin can race until the owner acts (equal rank) -
  the residual, stated.
- A member griefing by pre-registering: any admin or the root replaces it on
  the next sweep and may revoke it.
- A re-founded DeviceGroup / PublisherGroup: same root (same account seed), so
  a root reset re-claims the journal.
- Squatting a deterministic id: works only before the owner's first claim; the
  DeviceGroup is claimed at signup and a channel's publisher journal is claimed
  before the channel is announced (a client that cannot claim does not create
  the channel). Residual, stated.
- A free journal naming someone else's `push_gid`: the wake is dropped.

## Alternatives

- **A new journal per epoch.** Rejected (maintainer, 2026-09-28): the knock box
  and the push binding still need stable addresses protected the same way, the
  epoch boundary spans two journals, catch-up walks a chain, and the server can
  link the journals anyway.
- **Root reset only, anyone registers.** Rejected: an evictee blocks the group
  until the owner is online.

## Consequences

- Plain members no longer "heal" a missing epoch key; they wait for the
  committer's registration (register-before-publish already makes that rare).
- The DS learns, per journal, a root public key and a set of registrar public
  keys with ranks: pseudonymous (derived per group), not linkable to accounts
  or across groups. The push binding stores a keyed hash of the `push_gid`
  beside the journal; the DS already saw both in every woken publish (ADR-0054
  residual), so this adds a stored copy, keyed.
- Invariants: #2 (public keys and a keyed hash only); #3 untouched (derivations
  are client-side from existing seeds, no `lakko-mls` state-machine change);
  #4 (every refusal is a refusal, no downgrade); #5 (per-group pseudonymous
  keys; the push binding residual stated above).
- Flag day on the DS access tables: no production data.

## Plan

- **S1** `lakko-ds-proof` statements (claim, reset, register, revoke, registrar
  management, root transfer, push binding) + DS tables and rules + tests; FFI
  derivations and signers.
- **S2** `lakko-contacts` registrar-seed envelope + fuzz; clients: owner claims
  with root, appoints registrars on join / promotion, registers epochs with its
  registrar key, root reset on re-found, push binding on mint and rotation.
- **S3** docs, `lakko-security` re-run on the DS.
