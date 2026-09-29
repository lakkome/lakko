<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0057: A journal has an owner; only those who change a group register its epochs

- **Status:** Accepted (2026-09-28, the maintainer's call: option (a) of the
  security pass). **S1 built 2026-09-28**, with the core of S2 (the
  registrar-seed envelope + fuzz, both FFI surfaces, the NSE decode arm):
  host-tested against the local MariaDB. **S2 clients built 2026-09-28** on
  web, iOS and Android (host-tested on each), with the root handover on an
  ownership transfer (rank `root` in the envelope); see *What the S2 client
  work found*. Deployed to the test stack; not walked live.
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

## What S1 built (2026-09-28)

`crates/lakko-ds-proof/src/statement.rs` (derivations, statements, fuzz
`statement_decode`), `services/ds/src/ownership.rs` + migration
`0006_journal_ownership.sql` (flag day: `ds_group_access` recreated with
`registered_by` / `registered_rank`; `ds_group_owner`,
`ds_group_registrars`, `ds_push_bindings`), `crates/lakko-contacts/src/
registrar_seed.rs` (fuzz `registrar_seed_decode`), `ds_statement.rs` on
UniFFI and wasm, `registrar_seed.rs` on the NSE extension surface.
`LAKKO_DS_BINDING_PEPPER` (64 hex) is in the env templates and the test-stack
setup; unset, `PUT .../push-binding` answers `503 push-binding-disabled` and
every wake is dropped.

**Done differently from the text above, each for a reason:**

1. **The registrar seed is sealed to the addressee.** Section 1 sends it as an
   MLS application message "so only that epoch's members can read it" - but
   then every member reads every other member's seed, an admin's included,
   and the ranks mean nothing (any member could sign as an admin). The seed
   is sealed with the `lakko-contacts` sealed box to the member's **account
   X25519 key** (the `x25519_pubkey` of its identity bundle; every device of
   the account derives the secret from the account seed), the clear header
   (journal, addressee, rank) is the box's AAD. The owner therefore needs the
   member's identity bundle before appointing it.
2. **A claim and a reset are separate statements.** A same-root claim that
   names the epoch key already registered at that epoch is idempotent (`204`,
   the claim-on-open); any other same-root claim is `409`, and replacing the
   chain is an explicit `reset`. A client re-sending a stale claim can then
   never wipe the current chain by accident.
3. **The replace rule covers a suffix.** "`e == latest`, replacing" is widened
   to any `e <= latest` whose every registered epoch at or above `e` was
   registered by a strictly lower rank (the root: always); the epochs above
   `e` are dropped. An evictee that raced **two** epochs ahead
   (`N+1`, `N+2`) is thus replaced by the admin's `N+1` in one step, where the
   literal rule would have made `N+1` a root-only "anything else".
4. **Revocation is all-or-nothing.** A revoke list naming any registrar of an
   equal or higher rank refuses the whole statement (`403`), nothing written;
   a key that is no registrar is ignored.
5. **Order of refusals.** Shape `400` (undecodable, wrong kind for the route,
   another journal, epoch above `2^48 - 1`), then window + signature `401`,
   then - behind the group's lock row - a signer that is neither root nor
   registrar gets the same `401` **before its nonce is spent** (a stranger
   costs no row), then the nonce (`401` on a replay), then the rule (`403`
   rank, `409` conflict).
6. **Bounds, chosen not measured:** 256 keys per statement list, 4096
   registrars per journal.
7. **Push-binding squat, stated:** a current member that learns the
   `push_gid` before the owner binds it can bind it to a journal of its own
   first (`409` for the owner); the group recovers by rotating the routing
   secret (ADR-0043).

**Client contract (S2 client work):** every route is
`PUT /ds/group/<hex>/<segment>` with body `{"statement_b64": ...}` and no
proof header; the FFI signer returns `{path, body_json}`. The owner claims
with `dsJournalRootSigner(accountIdentity, groupId).claim(groupId, epoch,
epochKey)` (the epoch key is `dsAccessSigner(...).verifyingKey()`), appoints
registrars with `registrars(...)` and sends each seed with
`composeRegistrarSeedEnvelope`; members open it with
`openRegistrarSeedEnvelope(plaintext, accountIdentity)` and register epochs
with `dsStatementSigner(seed).registerEpoch(groupId, epoch, epochKey,
revoke)`; the committer of a removal lists the evictee's registrar key in
`revoke`. The ADR-0053 `groupClaimBodyJson` body is refused (`400`).


## What the S2 client work found (2026-09-28)

The three clients wired S2 against the contract above. Four things the text
did not settle, and what was decided for each:

1. **Ownership transfer must move the root, and the old owner cannot name
   the new root.** ADR-0045 `proposeOwnershipTransfer` changes the role
   table but not the journal: the new owner cannot appoint registrars,
   reset or bind, and the old owner keeps the root. The old owner cannot
   sign `root` naming the new owner's root key, because that key is derived
   from the new owner's account seed. **Decision: the old owner hands its
   root seed over.** When a device of the outgoing owner applies (or merges)
   the commit that carries its own transfer, it seals its root seed to the
   new owner's account X25519 key in the registrar-seed envelope at rank
   `root` (byte `3`, `lakko_ds_proof::ROOT_RANK_BYTE`) and posts it in the
   group. The new owner keeps it only while the live role table names it
   owner, signs `root` with it naming its own account-derived root, removes
   the old root's registrars (it can now derive them) and appoints every
   member again under its own root. Handing over the seed gives the new
   owner nothing it was not about to hold: it outranks every registrar the
   old root derived. The DS is the check on a forged envelope: a seed that
   is not the current root fails the `root` statement with `401`, so the
   receive gate does not need to trust the sender. Every device of the old
   owner may send it; after the first success the rest fail with `401`,
   which the new owner treats as done. Residual: until a device of the old
   owner comes online and applies the commit, the new owner is owner in the
   group and not on the journal (admins and registrars keep registering);
   a device of the old owner that lost its state cannot hand over.
2. **Re-founding a DeviceGroup resets the chain under the live devices.** A
   device recovered from the phrase that cannot rejoin re-founds the
   DeviceGroup under the same id and resets the journal; other devices of
   the account still in the old group lose write access. This is not new:
   two MLS groups under one id were already split. Recorded, not changed.
3. **A group with no role table has no owner, so nobody can register its
   epochs**, and its direct commits abort. `Group::create` has seeded a
   role table since ADR-0022 and there is no production data; recorded.
4. **Co-publishers get no registrar.** A PublisherGroup carries no
   application messages, so the channel owner has nowhere to send a
   registrar seed, and a co-publisher cannot register an epoch it advances
   (a rotation or an approval). Until a carrier exists, only the channel
   owner commits in a PublisherGroup. Unbuilt.

Also recorded: a lazy claim that meets `409` (the journal is already
claimed) does **not** make the owner register its current epoch with the
root, because a root registration replaces every epoch above it and an owner
device that is behind would cut the others off; the same holds after a lost
sweep. Nothing is re-registered.

## What the second security pass found (2026-09-28)

A `lakko-security` pass on the rules as the clients wired them returned
REQUEST CHANGES: four HIGH findings reopened the problem this ADR exists to
close. The maintainer chose the fixes recorded here; they amend the text
above.

1. **A member-rank registrar could freeze the journal.** `latest + 1` was open
   to every registrar with no bound on chaining, so two appends pushed every
   real key out of the write tier and 64 out of the read tail; the owner's
   lazy claim met `409`, admins never re-register, and every journal write
   was `401`, so no sweep could repair it. **Fix, server and client:** an
   epoch registration carries a mode. *Append* is accepted only at
   `latest + 1`; a member-rank signer may append `e` only when `e <= H + 1`,
   `H` being the newest epoch an admin or the root registered (so members'
   self-service commits cannot chain two epochs past the last admin or root
   registration; the pending proposal waits for one). *Replace* is the
   repair: it is permitted when every registration above `e` is of a lower
   rank than the signer and the one at `e` is lower rank or carries the same
   key; there is no root override. An owner or admin whose append met `409`,
   or whose signed request met `401` at its head, re-reads the journal after
   30 s and, if no commit moved the group past its epoch, replaces at its own
   epoch.
2. **An evicted member's registrar key survived.** Only a root revoked, and
   the owner removed registrars by a device-local ledger. **Fix:** a
   `registrars-set` statement (root only) replaces the whole registrar set;
   the owner sends the set derived from the live roster and role table, so
   every owner device converges the DS to the group without a ledger, and a
   root takeover resets the set in one statement.
3. **Claims failed open on web and Android.** A group or channel was created
   when its claim failed, and whoever claimed first became root for good.
   **Fix:** creation claims first and aborts on failure; no Welcome is sent
   and no channel announced before the claim lands.
4. **The root handover was one-shot and froze membership meanwhile.** **Fix:**
   the new owner keeps registering with its registrar seed until its takeover
   is confirmed; the outgoing owner keeps its owed record until a probe (a
   `root` statement naming its own root, `204` while it is still root, `401`
   once it is not) says the transfer happened, re-sending meanwhile; the
   receiver keeps the opened seed wrapped under its device key until the
   takeover succeeds. An outgoing owner that never hands over (malicious, or
   lost its state) stays root on the DS: the residual, stated.
5. **Push-binding squat on every mint and rotation**, not a narrow race: the
   binding followed the secret's broadcast. **Fix:** bind first, broadcast
   after.
6. (ADR-0049, recorded here with the rest) the device-link guess budget was
   client-side and the old device answered before anyone consented; see the
   ADR-0049 section 2 amendment.
7. (LOW) a lagging root device's sweep registration could truncate epochs
   other devices advanced. Closed by *Append* in every commit path and by the
   removed root override on *Replace*.
8. (INFO) the registrar table tells the DS each journal's member count and
   when members are appointed and dismissed. New metadata under invariant #5,
   pseudonymous per journal; accepted and stated.
