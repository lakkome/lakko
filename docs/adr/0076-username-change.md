<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0076: Choosing a username later, and changing it

- **Status:** Accepted (2026-10-13; the maintainer asked for both to be
  built; D1 and D2 are this document's recommendations, which the
  maintainer was shown and did not change). Built the same day on the core,
  `services/discovery`, `services/auth` and the three clients, unit-tested
  only; see "As built".
- **Date:** 2026-10-13
- **Amends (when built):** [ADR-0048](0048-profile-names.md) and
  [ADR-0060](0060-scoped-discovery.md) ("usernames are immutable"),
  [ADR-0054](0054-anti-abuse-without-client-work.md) (the session's handle
  bind), [ADR-0075](0075-recovery-email.md) (a rename keeps the recovery
  email).
- **UX:** `docs/ux/profile-names.md` (to be extended with the two screens).

Renumbered from 0068 on 2026-10-13: the main branch had taken 0064 to 0071 for
other decisions. Commit messages before that date name this ADR as 0068.

## Context

A username is set at sign-up and never again. Two gaps follow.

A **guest** (an account made by a call-link join, ADR-0032) has no username
and a random password nobody knows. It cannot be signed into from another
device, cannot add a recovery email and cannot add a device; every screen
that needs a name says "Choose a username first." and leads nowhere. The
server already supports the claim: it is the sign-up claim run later.

A **rename** is half there. `services/auth` replaces a login handle (and
drops the recovery email). `services/discovery` has no way to replace a
name: a second `register` adds a second row, the old name stays findable
and unclaimable forever, and `GET /discovery/username-certificate` keeps
signing the oldest. Nothing tells the account's other devices, and each of
them re-binds its stale name at its next landing, which silently reverts
the rename at auth.

## Decision

### D1. The old name is held for 30 days, then freed

After a rename nobody finds the person by the old name, and nobody else can
take it for 30 days. Freed at once, a stranger could take it the same hour
and be mistaken for the person; held forever, names are burnt.

### D2. One rename per 30 days

So an account holds at most one retired name at a time and cannot churn.

### 1. Discovery

- `usernames` gains `retired_at TIMESTAMP NULL`. A row with it set is
  **retired**: `lookup` and name search never answer for it, `is_public` is
  cleared, and it still occupies its hash, so `register` and `replace`
  answer `409` for it. An hourly pruner deletes rows retired more than 30
  days ago.
- `POST /discovery/register` refuses an account that already holds a
  current name (`409 username-already-set`), closing the two-row hole.
- New `POST /discovery/username/replace {account_id, oprf_hash}` (session,
  the per-account write limiter), one transaction under the account's row
  lock: the account must hold a current name (`409 no-username`); the last
  rename must be more than 30 days ago (`429 rename-too-soon` with
  `retry_after_secs`); insert the new row (`409` when the hash exists,
  current or retired, unless it is this account's own retired row, which is
  revived); retire the old row. Answers the new username certificate.
- `GET /discovery/username-certificate` and `PUT /discovery/visibility`
  act on the current row only.

The certificate format does not change. A certificate for the retired name
still verifies for whoever holds a copy; it is no longer served, and a peer
replaces it when the person's newer profile arrives (`profile_is_newer`).
Stated as a residual.

### 2. Auth

- `PUT /auth/login-handle` accepts an optional
  `recovery_email: {address, vault_salt?, sealed_entropy?}`. When present
  and the account has a recovery email, the server checks the address
  against the stored key under the OLD handle, writes the key under the NEW
  handle and, when a vault is sent and no phrase is saved, the vault, in
  the same transaction, instead of dropping them. No letter: the address is
  the confirmed one. A mismatch refuses the whole bind
  (`403 address-mismatch`). Absent, the rows are dropped as today.
- A handle CHANGE (not a first bind) needs the fresh-auth grant, in the
  body: a session alone must not move the name the account is signed into
  by.
- New `POST /auth/login-handle/check {handle}` (bearer) answers
  `{state: "same" | "other" | "none"}`. The session sync on every client
  binds only on `none`; on `other` it binds nothing and waits for the name
  from the account's other devices.

### 3. Between the account's devices

The username becomes one more register of the synced state (`lakko-sync`,
the recovery-email register's shape: LWW, a DeviceGroup service envelope,
in the backup). A device that applies a newer name updates its local
account marker and session state and re-derives what depends on it.

### 4. Client order

Claim (guest): local validation, `PUT /auth/login-handle`, discovery
`register` (on failure release the handle), keep the returned certificate,
local marker and register, first password (the device's seed-based
change), profile re-publish, then the protect-your-account step.

Rename: warning and proof block; `PUT /auth/login-handle` with the new
handle, the grant and, when the device holds the address, the recovery
email re-key (vault re-sealed under the new handle); discovery `replace`
(on failure bind the old handle back with the same re-key); certificate
from the response; local marker and register; profile, public record,
introduction record and member cards re-published. A device that does not
hold the address warns that the recovery email will be turned off.

## Stated costs

- A copy of the old name's certificate verifies forever.
- Mentions of the old name in old messages stop matching; a passkey's label
  in the authenticator keeps the old name.
- A device offline during the rename keeps the old name until it syncs; its
  pairing guard compares against the old handle until then.
- The old name's login handle is free at auth at once (auth cannot hold it
  without knowing which discovery name it belongs to); a stranger who binds
  it still cannot register the name at discovery for 30 days and releases
  it, as at any failed sign-up.

## Invariants

#1, #2, #5 hold: discovery still stores one-way lookup keys only, and the
new register and envelope travel between the account's own devices,
end-to-end encrypted. #3 untouched.

## Sprint plan

1. Core: the username register in `lakko-sync`, FFI.
2. `services/discovery` (migration, replace, retired rows, pruner) and
   `services/auth` (re-key on bind, the grant on a change, the check
   route), DB-gated tests.
3. Web, iOS, Android: the claim (already in progress with ADR-0075's
   follow-up), then the rename.

## As built (2026-10-13)

Unit tests only: `lakko-sync` 96, `lakko-ffi-uniffi` 343, `lakko-ffi-wasm`
230, auth 196 + 2 and discovery 121 against a local database, web vitest
4999, iOS `LakkoTests` 3468 (0 failures, 2 skipped), Android 3662 on the
host JVM without the native core. Nothing was walked on a device or against
a server. In repeated discovery runs one or two older identity-bundle and
key-reset tests failed intermittently (one run in six); the cause was not
established and another session may have been using the same test database.

- **Core:** the own-username register (`synced/own_username.rs`, entry tag
  12, envelope domain `lakko/sync/username/v1\0`), validated with the one
  username rule; like the recovery-email register it rides persistence and
  the backup but not `snapshot()`, so a client re-sends its envelope on
  DeviceGroup growth.
- **Discovery** (migration `0028`): as section 1, plus a database-enforced
  "one current name per account", a `username_renames` table for the
  interval, JSON 409 bodies on `register`, and knobs
  `LAKKO_DISCOVERY_RENAME_INTERVAL_SECS` /
  `LAKKO_DISCOVERY_RETIRED_NAME_HOLD_SECS` (30 days each, chosen). **A
  rename removes the account's name terms and public record**, which were
  keyed over the old name; the client publishes them again.
- **Auth:** as section 2; the re-key needs `previous_handle` in the body
  (the server stores only the keyed handle); `handle-mismatch` and
  `address-mismatch` refuse the whole bind; the check route has its own
  counter.
- **Clients:** the guest claim (name and first password on one screen, the
  password set by the seed-based change while a per-account "first password
  owed" flag is set), the rename as one owed operation with a bind-back on
  refusal, the check-first session sync, and the register.

Found while building, and now the rule on all three clients: **a device
seeds an empty register from its local name only after auth confirmed that
name's handle in the session.** Seeding unconditionally would let a device
that had not yet heard of a rename write its old name with a newer stamp
and undo the rename everywhere.

Open:

- A roll-back at a landing cannot run without a fresh-auth grant (binding
  the old handle back is a change). The owed record waits for the next run
  that holds one; meanwhile auth may hold the new handle while discovery
  holds the old name, and nothing tells the person. The handle sync answers
  `other` on that device and binds nothing.
- A device waiting on `other` re-checks only at its next session start or
  refresh.
- Channel member cards are not re-published by the rename itself on iOS and
  Android.
- The rollback view on web has no text of its own.
