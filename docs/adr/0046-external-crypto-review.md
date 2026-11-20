<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0046: External cryptographic review — a programme gate, not a per-change blocker

- **Status:** Accepted
- **Date:** 2026-09-01
- **Context:** Security invariant #3; the accumulated `lakko-mls` change surface
- **Relates to:** [ADR-0015](0015-device-removal-authorization.md),
  [ADR-0022](0022-group-admin-roles.md), [ADR-0023](0023-message-edit-delete-reactions.md),
  [ADR-0045](0045-remote-commit-epoch-fork.md)

## Context

Security invariant #3 reads, in full:

> **The MLS state machine is sacrosanct.** Any change to `lakko-mls` requires an ADR +
> property tests + (after Phase 2) an external crypto audit.

In practice the third clause has been carried **inside each ADR that touches
`lakko-mls`**, as an open item on that ADR, and again in the invariant index beside each
`C-` entry the change minted. Some version of *"the external crypto review this ADR
requires has not happened"* appears in [ADR-0040](0040-durable-pending-commit-outbox.md)
and [ADR-0045](0045-remote-commit-epoch-fork.md), and in **four** rows of
[`invariant-index.md`](../security/invariant-index.md) (C-63 / C-64 / C-65 / C-66) plus
their mirrors in [`calls.md`](../security/calls.md); the obligation is also referenced,
without being made a blocker, by [ADR-0012](0012-ech-hpke-adapter.md),
[ADR-0014](0014-webauthn-action-binding.md), [ADR-0015](0015-device-removal-authorization.md)
and [ADR-0022](0022-group-admin-roles.md).

That reading has a consequence nobody chose: **no ADR touching the MLS state machine can
ever be Accepted, no matter how finished it is.** ADR-0045 is built end to end across the
core, both FFI surfaces and three clients; it has been through a `lakko-security` pass
that returned REQUEST CHANGES on seven findings, all of which are closed; it carries 242
core tests and seven indexed invariants. It is as finished as a feature gets. It is also,
under the per-ADR reading, permanently `Proposed` — and so are its neighbours, for a
reason that has nothing to do with any of them individually.

Worse, the debt is invisible in aggregate. Spread across four documents as four sentences,
nothing states what an auditor would actually be asked to look at, so the obligation is
simultaneously blocking everything and specified nowhere.

**The invariant does not say what the per-ADR reading assumed.** It scopes the audit
with *"after Phase 2"* — a **programme phase**, not a per-change precondition. An ADR is
a record of a decision and its consequences; a cryptographic review is a periodic
examination of an accumulated surface by someone outside the project. Those are different
artifacts on different clocks, and binding the first to the second was a misreading rather
than a policy.

## Decision

**The external cryptographic review is tracked once, as a programme gate, in one place.
Individual ADRs stop carrying it as a blocker.**

Concretely:

1. **Invariant #3's text does not change.** The per-change half — an ADR plus property
   tests, before the change lands — stays exactly as it is and stays enforced per change.
   Nothing here lowers the bar for touching `lakko-mls`.
2. **The audit obligation moves to a single tracked gap, G-39**, which owns the scope,
   the accumulated surface, and the trigger. It is the only place the debt is stated.
3. **An ADR touching `lakko-mls` may reach `Accepted` on its own merits** — decision
   recorded, consequences known, built, property-tested, and where the change warrants it,
   through a `lakko-security` pass. Each such ADR carries a pointer to G-39 instead of its
   own copy of the debt.
4. **The gate is a release gate.** No build reaches users — no store submission, no
   GA tag — while G-39 is open. That is where "after Phase 2" bites, and it is the
   sentence that keeps this from being a way to avoid the audit. **Amended 2026-11-18:
   a capped public beta may run before the review; see the amendment below.**

## Amendment (2026-11-18): a capped public beta before the review

**Decided by the maintainer.** The review is not funded, and the funders who pay for one
(OTF Security Lab among them) want a project that people already use. A project that may
not reach users until it is audited cannot show users to the people who would pay for the
audit. Decision 4 is therefore narrowed: G-39 gates **leaving the beta**, not **any**
contact with users.

What is allowed while G-39 is open:

1. **A public beta capped at 1000 accounts**, the cap enforced by the server, not by a
   promise. Guest accounts from call links (ADR-0032) that are swept within a day do not
   take a place. **Built 2026-11-19** (`services/auth/src/account_cap.rs`, migration
   `0038`): an account takes a place when it binds its first username (a row in
   `login_handles`, counted under a one-row lock); past `LAKKO_AUTH_MAX_ACCOUNTS` that
   bind is `403 registration-closed`, `GET /signup/open` answers before the proof of
   work, a rename is never refused, and a deleted account frees its place. UX spec
   [`docs/ux/beta-account-cap.md`](../ux/beta-account-cap.md); the three clients built the
   same day, unit-tested, not walked.
2. **Beta distribution channels only:** the web client, TestFlight external testing,
   Google Play closed or open testing, and the desktop build downloaded from lakko.me.
   No production store listing.
3. **Said plainly where a person decides to use it:** the sign-up screen, the website and
   the store beta descriptions state that Lakko is a beta, that its cryptography has not
   been independently audited, and that it is not yet meant for people whose safety
   depends on it. UX spec [`docs/ux/beta-notice.md`](../ux/beta-notice.md) (2026-11-19):
   a banner on the first signed-out screen, the sign-up screen and the guest's create-account
   form, and the first row of Settings > About, built on the three clients the same day
   (unit-tested, not walked); store text drafted in
   [`docs/beta-store-text.md`](../beta-store-text.md).
4. **Not promoted to people at risk** (activists, journalists, people in countries that
   block messengers) until the review is done. The censorship-resistant transports stay
   in the build; the beta is simply not advertised for that use.
5. **A way to report a vulnerability exists before the first user signs up**
   (`SECURITY.md`, a published address and key).

What still waits for G-39, unchanged: lifting the cap, a production store listing, a GA
tag, and dropping the word "beta". The risk this amendment accepts is real: up to 1000
people use cryptography that only its author and an internal review have examined. It is
accepted because the alternative is that the review is never funded; it is bounded by the
cap and by saying so.

## What this explicitly does NOT do

- **It does not relax invariant #3**, reduce the audit's scope, or make it optional. The
  obligation is unchanged in content and unchanged in force; only its *location* moves,
  from four scattered sentences to one specified item.
- **It does not lower the per-change bar.** An ADR and property tests are still required
  before a `lakko-mls` change lands, and `lakko-security` remains available on request and
  was worth its cost — it found three HIGH findings and a confirmed impersonation in a
  surface that had already shipped.
- **It does not claim the reviewed surface is smaller than it is.** It is larger than any
  one ADR: six core sprints of ADR-0045, plus C-63 through C-69, plus three independent
  client implementations of the ordering and rollback disciplines the core change depends
  on. G-39 enumerates it.
- **It does not make an unaudited change safe.** It makes an unaudited change *shippable
  to nobody*, which is what the phase scoping already meant.

## Consequences

- The ADRs and index rows carrying the debt lose it and gain a pointer to G-39 instead.
  **One correction to how this bullet originally read**, because it overstated the
  blocker's reach: it named ADR-0015, ADR-0022, ADR-0023 and ADR-0045 as *"permanently
  `Proposed`"*, and only **ADR-0045** actually was. ADR-0015 and ADR-0022 reached
  `Accepted` while still carrying a mention of the audit, and ADR-0023 never mentioned it
  at all — which sharpens rather than weakens the case here: a blocker that three of four
  documents quietly stepped over was already not functioning as one, and was doing its
  only real work against the single most finished ADR in the set. Whether ADR-0045 flips
  to `Accepted` remains the maintainer's call, made on that ADR's own merits. **As of
  2026-09-02 the pointers have landed in ADR-0040, ADR-0045 and the four invariant-index
  rows; ADR-0012 / ADR-0014 / ADR-0015 / ADR-0022 still carry their own wording.**
- The audit acquires, for the first time, a written scope — which is the thing an
  external reviewer actually needs and which four scattered sentences never provided.
- The release gate becomes explicit and checkable, where before it was implied by a
  clause in an invariant and enforced by nothing.
- **Risk, stated plainly:** a single tracked item is easier to defer than four
  blockers each stopping its own document. That is the real cost of this decision. The
  mitigation is that the release gate is unambiguous and states a consequence nobody can
  quietly route around — and that the debt is now specified, so deferring it is a visible
  choice rather than a default.

## Alternatives considered

**Keep the per-ADR blocker.** Rejected because it is what produced the current state: no
MLS-touching ADR can ever be Accepted, the debt is unspecified, and the signal is
worthless precisely because it is everywhere. A blocker that blocks unconditionally and
forever stops being read as a blocker.

**Drop the requirement.** Not seriously considered. The `lakko-security` pass on ADR-0045
is the argument against it: an internal review of an already-shipped surface found three
HIGH findings and a confirmed impersonation, none of which needed an attacker and four of
which needed nobody at all. An external review has a strictly better chance of finding
what an internal one shares blind spots with.

**Audit per-ADR, for real.** The honest version of the current reading: commission a
review for each MLS-touching change before accepting it. Rejected on both cost and shape
— a cryptographic review examines an interacting surface, and the interactions between
ADR-0022's role table, ADR-0045's proposal gate and C-63's commit rule are exactly what a
per-change review would each see only a slice of.
