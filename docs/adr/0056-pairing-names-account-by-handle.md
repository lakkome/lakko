<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0056: A fresh device names its account by login handle, not by account id

- **Status:** Accepted (2026-09-28, the maintainer's call); S1 and S2 built and deployed to the test stack 2026-09-28; server side checked there, the pairing itself not yet walked live
- **Date:** 2026-09-28
- **Context:** Phase 3 (product hardening); a consequence of
  [ADR-0054](0054-anti-abuse-without-client-work.md) S3 and S5
- **Amends:** the pairing payload of ADR-0007 Phase 2a
  (`schemas/wire/device_pairing_v1.proto`, `lakko_mls::pairing`), the
  ADR-0018 grant redeem and the ADR-0049 device request

## Context

A fresh device joins an account through another device of that account: it
shows a QR (ADR-0007) or posts the same bytes to `POST /device/request`
(ADR-0049), the signed-in device confirms, mints a grant (ADR-0018), seals the
grant and the phrase seed to the fresh device, and the fresh device redeems the
grant for a session.

Three steps name the account by its 16-byte `account_id` before the fresh
device has a session:

1. the pairing payload carries `account_id`, and the confirming device refuses
   a payload whose id is not its own (`pairing::decode(bytes,
   expected_account_id)`), so a device meant for account A is never enrolled
   into account B by mistake;
2. `POST /device/request` takes `account_id` to route the prompt to that
   account's devices;
3. `POST /device/authorize/redeem` takes `account_id` beside the grant.

The fresh device learned the id by a username lookup. Since ADR-0054 S3 a
lookup needs an anonymous token, and a token needs a session, so a device with
no cached account id can no longer start this path. ADR-0054 S5 gave sign-in by
name a **login handle** (`BLAKE3-derive-key("lakko/auth/login-handle/v1",
canonical username)`, 32 bytes), which auth resolves itself, enumeration-safely.

## Decision

**The fresh device names the account it wants by the login handle of the
username the person types; nothing before a session carries the account id.**

1. **Pairing payload v2.** `PairingQr` gains `bytes login_handle = 5` and
   `version = 2`; field 2 (`account_id`) is reserved and never reused. The
   decoder refuses any version other than 2 and any handle that is not 32
   bytes. `pairing::decode(bytes, expected_login_handle)` compares the handle
   (constant time) with the confirming device's own handle, computed from its
   own username through the one `login_handle` function. The guard keeps its
   meaning: a payload meant for another account's name is refused before any
   MLS commit. A confirming account with no username (a guest) cannot confirm,
   which matches a fresh device that has no name to type.
2. **`POST /device/request`** takes `{handle, pairing_payload_b64}`. Auth
   resolves the handle as S5 does; an unknown handle is stored against its
   stable stand-in account, which no device polls, so the request simply
   times out and the answer never reveals whether the name exists. The
   per-target prompt counter keys on the handle.
3. **`POST /device/authorize/redeem`** takes `{grant_token_b64}` only. The grant
   is 32 random bytes whose SHA-256 is stored against the account, so the hash
   finds the account; the response already returns `account_id_b64`, which is
   where the fresh device learns it, after proving the grant.

No dual path: a v1 payload and the old bodies are refused (no production
deployment).

## Alternatives

- **A pre-session resolve that returns the account id** (real, or a stand-in
  for an unknown handle). Rejected: an account id is a UUIDv7 whose leading
  bits are its creation time, so a stand-in must fake a plausible creation
  time; any fixed range is distinguishable from real accounts created later,
  which turns the route into an existence oracle.
- **The confirming device fills in the account id.** Rejected: the payload is
  shown or posted before any device confirms, and the prompt still has to be
  routed to the right account; the handle solves both at once.
- **Drop the account check from the payload.** Rejected: it is the guard
  against enrolling a device into the wrong account, and the handle keeps it
  for free.

## Consequences

- Another-device recovery works again on a device with no cached account, by
  typing the username.
- The pairing codec changes: a `lakko-mls` change under invariant #3 - this ADR,
  property tests (round trip, wrong handle refused, v1 refused, wrong length
  refused) and the existing `pairing_qr` fuzz target extended to the handle.
  Recorded for G-39.
- Invariants: #1 unchanged (a handle is not PII; auth already holds it
  peppered); #2 unchanged (the payload stays opaque to the server); #5
  unchanged (the request is routed to the account the name already resolves to
  for sign-in); #4 unchanged.
- A person must know their username to add a device this way, as before.

## Plan

- **S1** proto v2, `lakko_mls::pairing` encode/decode by handle, property tests,
  fuzz; auth `POST /device/request` by handle, redeem by grant; both FFI
  surfaces; bindings.
- **S2** web, iOS, Android: the fresh device computes the handle from the typed
  username (no lookup), the confirming device passes its own handle to decode,
  redeem sends the grant only.

## What S1 built (2026-09-28)

Host-tested (the auth tests against the local MariaDB); not deployed, no
client moved yet.

- **Proto.** `PairingQr.login_handle = 5`; `reserved 2; reserved
  "account_id";`; the free range is `6 to 15`. `PAIRING_VERSION = 2` and the
  decoder refuses every other version, 1 included.
- **Constant-time compare** through `lakko_crypto::secret::SecretBytes`,
  whose equality is `subtle::ConstantTimeEq`: `lakko-mls` already depends on
  `lakko-crypto`, so no dependency was added. The handle is a public function
  of the name, so this is the defence in depth the decision asks for, not a
  secrecy property.
- **An unknown handle's request is stored, and that needed a migration.**
  Before S1 the insert against an unknown account id failed on the
  `device_requests` foreign key and was folded into the same `200`, so the
  response matched but `POST /device/request/status` then answered
  `pending: false` for an unknown name and `true` for a known one: an
  existence oracle. The decision's "stored against its stand-in" closes it
  only if the row is actually written, so migration `0022` drops that foreign
  key; account deletion now clears the account's request explicitly in the
  same transaction, and the 10-minute TTL bounds anything left behind. The log
  line for a posted request no longer carries the account id (for an unknown
  name it would be the stand-in).
- **Redeem by grant.** `device_grants` is keyed `(account_id, grant_hash)`,
  so migration `0022` adds a unique index on `grant_hash`. The consume is a
  locking read of the live row and a delete of exactly that row, in one
  transaction (the `opaque::take_login_state` pattern). There was no per-name
  counter on redeem to re-key: it is per-address only, and stays so, since a
  counter keyed on a presented hash would be a bucket no second request ever
  shares.
- **Fuzz.** The `pairing_qr` target binds against a fixed handle and asserts
  that anything accepted carries version 2 and exactly that handle.

## What S2 found (2026-09-28)

`DeviceGroup.joinFromWelcome(welcome, identity, account_id)` checks up front
that the Welcome's group is the account's DeviceGroup, but the fresh device now
learns the account id only from the redeem, and the grant it redeems is sealed
under the joined group's exporter key. The clients therefore join with the
plain `Group` join (the same exporter), open the recovery row, redeem, and only
then apply the same check against the redeemed account id (derived DeviceGroup
id equals the joined group id; on mismatch nothing is persisted and the seed is
zeroed). Where a session already knows the account, a redeem answering for
another account is refused.

For the security pass: the check moved after the redeem. The account behind a
name was already the server's word before this ADR (the discovery lookup), so
a malicious server that routes the request to its own device and confirms it
lands the fresh device in that account in both designs; what protects the
person is that their own device shows no prompt and the comparison code is
never shown to them. A post-redeem cross-check (look the name up with the new
session and compare) would ask the same server again and adds nothing against
it.
