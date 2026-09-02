<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0033: Call-admission anti-spam throttle for guest-account link joins

- **Status:** Accepted (Variant A — owner-suspension deferred behind a
  hard-to-forge signal; end-call + throttle is the automatic response).
  **Implemented** — this ADR carried no implementation record until the
  2026-09-02 documentation audit added one: `services/auth/src/throttle.rs`
  with migration `0014_call_admission_throttle.sql`
  (`call_admission_events` + `throttled_calls`), gating `POST /call/jwt`. Every
  knob defaults safe (limit 0 = disabled), and `lakko-security` returned
  APPROVE-WITH-NOTES with four low/info findings, all ADR-accepted. The
  **owner auto-suspension remains deliberately unbuilt** for the
  weaponisability reason below — that is a standing decision, not a backlog
  item.
- **Date:** 2026-07-15
- **Context:** [ADR-0032](0032-multi-joiner-call-links.md) residual — guest accounts
  on public `/c/<slug>` call links open an account-creation-spam surface.

## Context

ADR-0032 lets an anonymous `/c/<slug>` visitor auto-create a full account
(`signupGuest`) to join a call. `lakko-security` flagged (low) that this is an
account-creation-spam surface: a burst of guest joins on one link mints many
accounts (each = an account row + a published identity bundle + a KeyPackage + a
best-effort KT-log append). `signupGuest` reuses the PoW-gated `/signup` path, so
this is friction-lowering, not a new endpoint — but the one-tap public-link flow
warrants a real throttle before a public production launch. ADR-0032 named "an
intelligent throttle at the call-admission / auth layer" as the deferred
mitigation; this ADR specifies it.

The recon confirms the natural hook: `POST /call/jwt` (`services/auth`) is
bearer-authed and already sees `(sub = account_id, call_id)` per admission, and
the `accounts` table carries an indexed `created_at` — so "is this a brand-new
account?" and "how many new accounts joined this `call_id` recently?" are both
cheap to answer at the admission point.

## Decision

**Throttle new-account call admissions per `call_id` at `POST /call/jwt`.** On each
mint request the auth service:

1. Reads the requesting account's `created_at`; if it is within a **guest window**
   (`LAKKO_AUTH_GUEST_ACCOUNT_AGE_SECS`, default e.g. 3600 s) the account counts as
   a **fresh/guest** account.
2. Records `(call_id, account_id, now)` and counts the **distinct fresh accounts**
   that requested admission to that `call_id` within a **sliding window**
   (`..._WINDOW_SECS`). If the count exceeds a **burst threshold**
   (`..._BURST_LIMIT`), the `call_id` is marked **throttled** (a `throttled_calls`
   row with a TTL, `..._THROTTLE_SECS`).
3. **A throttled `call_id` refuses ALL further admissions** (429) until the TTL
   expires — this is the **"end the call"**: no new joins, and the existing
   participants' short-TTL JWTs (already minted) age out, so the call dies rather
   than growing. No SFU room-drop signal is needed in V1 (the short JWT TTL is the
   lever); a direct SFU "terminate room" control is a later optimisation.
4. **Owner action.** The `call_id` alone does not identify the owner server-side
   (the auth service is not in the call's MLS group), so V1 records the throttle
   event for **operator review** rather than an automatic account suspension.
   Automatic **owner-suspension** is gated on a **hard-to-forge abuse signal**
   (App Attest / Play Integrity / a device-bound attestation / a raised PoW cost on
   the guest mint) — NOT the raw `(fresh accounts, call_id)` counter, because that
   counter is attacker-controlled: anyone who has the link can flood it. The
   maintainer's framing is that a call link is shared privately with intended
   participants, so a *stolen*-link flood is unlikely; this ADR therefore keeps the
   end-call + throttle as the automatic response and leaves auto-suspension to the
   hard-signal follow-up (deferred), so the throttle can never be weaponised into
   suspending a victim.

**Load-bearing properties:**

- **The abuse signal is account-AGE × `call_id`** — "many brand-new accounts on
  ONE link, fast." A legitimate ad-hoc call is a handful of participants, mostly
  not brand-new; a spam flood is a burst of fresh mints on one link. Keying on
  both avoids throttling (a) a large legit call of existing users and (b) organic
  new-user growth spread across many links.
- **No E2EE / invariant impact.** The throttle is an **availability gate on
  admission** — it sees only `account_id`, `call_id`, and timestamps (bookkeeping,
  invariant #2 — no keys, no message/media content) and never touches the E2EE
  path (#4 unaffected — a throttled admission is a refused JWT, not a plaintext
  fallback).
- **Fail-OPEN on throttle-infra error.** If the throttle store errors, the handler
  **mints** the JWT (does not block). The throttle is anti-abuse, not a security
  invariant; failing closed would turn a throttle-DB blip into a self-DoS on all
  calls. A false-negative is a spam window; a false-positive is a denied legit
  call — so lean permissive. (This is the deliberate exception to the project's
  usual fail-closed default: it applies to security gates, not to an availability
  throttle.)
- **Client-transparent.** No client change: a throttled join surfaces the existing
  429 path as "this call is not accepting new participants — try later."

## Alternatives

- **PoW-harden the guest mint (raise the Equix effort).** Complementary — keep a
  higher `LAKKO_POW_EFFORT_BITS` for the guest-signup path as a knob — but a
  determined attacker (or a botnet) pays the cost; PoW raises the price, the
  throttle caps the blast radius per link. Not a substitute.
- **Per-IP rate-limit.** Rejected as the primary signal: evadable via proxies and
  it harms legitimate shared-NAT users; usable only as a coarse secondary hint.
- **Automatic owner-suspension on the raw counter.** Rejected for V1 —
  weaponisable (flood a link → suspend its owner) and the auth service cannot even
  identify the owner from `call_id`. Deferred behind a hard-to-forge signal.
- **No throttle.** Rejected — it is the named ADR-0032 residual and the reason the
  guest-account flow is not yet safe for a public launch.

## Consequences

- Service surface (`services/auth`): a migration for a `call_admission_events`
  table (`call_id`, `account_id`, `at`, indexed for the windowed distinct-count) +
  a `throttled_calls` table (`call_id`, `until`), both pruned by a background loop;
  the `issue_call_jwt` handler gains the age-check + windowed count + throttle
  gate (fail-open); config knobs (`LAKKO_AUTH_GUEST_ACCOUNT_AGE_SECS`,
  `..._ADMISSION_WINDOW_SECS`, `..._ADMISSION_BURST_LIMIT`, `..._THROTTLE_SECS`),
  all with safe defaults + an off switch (limit 0 / unset = disabled). Integration
  tests: a burst trips the throttle + a throttled call refuses; a legit few-user
  call is unaffected; a throttle-store error fails open.
- No client change; no MLS / crypto change; no SFU change in V1.
- **Invariants:** #2 (server opacity — admission bookkeeping only, no keys) and #4
  (no E2EE impact — availability gate, and it fails OPEN by design). This ADR + the
  tests are offered to `lakko-security` (it touches the admission path).
- Deferred: the hard-to-forge-signal automatic owner-suspension; a direct SFU
  "terminate room" control (V1 relies on the short JWT TTL); tuning the thresholds
  under a real load test.

## References

- [ADR-0032](0032-multi-joiner-call-links.md) — the guest-account flow whose
  spam surface this throttles (its "UX (V1)" + Finding-1 residual).
- `services/auth/src/call_jwt.rs` (`POST /call/jwt`, the throttle hook) +
  `services/auth/migrations/0001_init.sql` (`accounts.created_at`).
- `crates/lakko-pow-server` — the complementary PoW knob on `/signup`.
- G-21 / G-22 (`docs/security/known-gaps.md`) — the ADR-0032 arc this completes.
