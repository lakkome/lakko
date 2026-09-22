# Finding format, evidence standard and severity rubric

Used by every workstream when filing, and by `audit-verify` when calibrating.
A finder proposes a severity; the verification pass sets it.

---

## Finding identifiers — two stages, because workstreams file in parallel

Six workstreams write into one `findings/` directory at the same time, so a single
running number would collide. Identifiers therefore have two stages, which is also
how a review firm works: an internal tracking id at filing, a report id at
publication.

**At filing**, the finder uses its own workstream prefix and numbers within it:

| Workstream | Prefix | Example |
|---|---|---|
| `audit-recon` | `RC` | `LAK-RC-01` |
| `audit-crypto` | `CR` | `LAK-CR-01` |
| `audit-protocol` | `PR` | `LAK-PR-01` |
| `audit-impl` | `IM` | `LAK-IM-01` |
| `audit-clients` | `CL` | `LAK-CL-01` |
| `audit-backend` | `BE` | `LAK-BE-01` |
| `audit-impl`, tooling half | `DY` | `LAK-DY-01` |

**At consolidation**, `audit-verify` assigns the report id — `LAK-01..` in severity
order — and records the mapping in `20-verification.md`. The filing id stays in the
finding file as `Tracking id:` so every note and every cross-reference written during
phase 3 still resolves after the renumber. A merged finding carries **all** of its
filing ids, which is how the report shows that two workstreams converged on it
independently.

## The finding file

One file per finding at `docs/audit/<engagement>/findings/LAK-<XX>-<NN>.md`,
renamed to the report id at consolidation. Markdown files under `docs/` carry the
SPDX header, as everything in this repository does.

```markdown
<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# LAK-07 — A stale role table lets a demoted admin freeze the sweep

- **Severity:** Medium        (set by the verification pass)
- **Confidence:** CONFIRMED   (CONFIRMED | PLAUSIBLE)
- **Workstream:** protocol
- **Tracking id:** LAK-PR-03  (the filing id; `LAK-07` was assigned at consolidation)
- **Status:** Open            (Open | Fixed | Partially fixed | Not fixed | Risk accepted)
- **Component:** `crates/lakko-mls/src/group_proposals.rs:412-455`
- **Invariants touched:** #3, #4, I-4, C-65, G-38
- **Vendor claim contradicted:** ADR-0045 §"What the client sprints found" (e)

## Preconditions
What must be true for this to happen. Be exact about who the actor is and what
they already hold. If the answer is "nothing — it happens on its own", say that
first and in those words: it is the most important sentence in the finding.

## Mechanism
The defect, traced through the code. Every step cites `path:line` and quotes the
few lines that carry the step. A reader must be able to follow it without opening
the repository, and must be able to check it by opening the repository.

## Reproduction
A concrete sequence: inputs, ordering, state. Where a test can demonstrate it,
give the test — as a snippet in this file, not as a file added to the crate.
Where it cannot be run here, say which capability is missing.

## Impact
What an attacker (or an unlucky user) gains or loses. Bound it honestly: name the
thing that does *not* break as well as the thing that does.

## Recommendation
What to change and why that closes it. If there are two reasonable fixes, give
both with the trade-off. Do not write the patch.

## Notes
Anything that qualifies the above: uncertainty, an adjacent case not checked, a
reason the severity is lower than it first looks.
```

---

## Confidence

| Value | Means |
|---|---|
| `CONFIRMED` | The mechanism is traced end to end through code read this session, and the verification pass failed to kill it. |
| `PLAUSIBLE` | The mechanism is coherent and the code supports it, but settling it needs something unavailable here (a device, a deployed service, a long fuzz run, a platform toolchain). The missing step is named in the finding. |

Nothing else ships as a finding. A suspicion that survives neither goes to the
**open questions** appendix, which is a legitimate deliverable — it tells the
vendor where a reviewer's attention snagged.

---

## Severity

Severity is `Impact x Reachability`, then adjusted by the rules below.

### Impact

| Level | Lakko-specific anchor |
|---|---|
| **Catastrophic** | Message plaintext or key material recoverable by the server, the relay, a media SFU or a network adversary. Impersonation of another account. Silent downgrade to non-E2EE. Forged identity binding accepted by a peer. |
| **Severe** | Cross-user integrity break — rewriting, deleting or mis-attributing another member's content. Permanent unrecoverable state: a group that can never change membership again, an account that can never be recovered. A removed member still able to read or be routed to. |
| **Moderate** | The social graph or an account's identity materialised somewhere invariant #5 says it is not. Privilege escalation inside a group beyond the role held. Denial of service recoverable only by an out-of-band action. Metadata that lets a provider cluster or track users. |
| **Minor** | Defence-in-depth weakened without a reachable break. Local-only exposure. Availability annoyance that self-heals. Documentation that misstates a security property. |

### Reachability

| Level | Means |
|---|---|
| **Unconditional** | No attacker required. Time passing, a network blip, an ordinary user action, two admins doing normal things concurrently. |
| **Unauthenticated remote** | Any network position or the server itself. No account needed. |
| **Authenticated remote** | Requires an account, or membership of the target group, or a link that was shared. |
| **Privileged or local** | Requires an admin role the design already trusts, device compromise, or physical access. |

### Matrix

|  | Unconditional | Unauth. remote | Auth. remote | Privileged/local |
|---|---|---|---|---|
| **Catastrophic** | Critical | Critical | Critical | High |
| **Severe** | Critical | High | High | Medium |
| **Moderate** | High | Medium | Medium | Low |
| **Minor** | Low | Low | Low | Informational |

### Adjustment rules — apply after the matrix

1. **Unconditional reachability is never softened for being "unlikely".** Lakko's own
   internal review found seven issues of which four needed nobody at all. That class
   is the one external review exists to catch; do not discount it because no
   adversary appears in the story.
2. **A `PLAUSIBLE` finding is capped at High.** Critical asserts certainty.
3. **A finding whose only actor is already-assumed-compromised** (on-device malware
   where the design says the device is trusted) **is capped at Low**, unless it
   crosses a boundary the design explicitly claims to hold against that actor.
4. **A finding that a vendor document already states as an accepted residual** is
   filed anyway, at the severity the mechanism earns, with the vendor's acceptance
   quoted in *Vendor claim contradicted* — and the report notes it is disputed
   rather than new. Do not silently defer to the vendor's acceptance: the value of
   the review is in disagreeing where disagreement is warranted.
5. **Fail-open decisions are judged as a set, not one at a time.** Several of Lakko's
   fail-open choices are individually argued and jointly untested. A single one is
   usually Low; a composition that yields a downgrade path is not.
6. **Severity describes the defect, never the effort to fix it.**

---

## What is not a finding

Filed instead as **hardening observations** (still in the report, own section):

- Style, naming, clippy-level nits, missing documentation.
- A stricter-than-necessary construction that is merely unusual.
- A gap the vendor has already scoped, documented and not yet built, where you
  agree with both the scoping and the priority — note it, do not inflate it.
- Anything you would not defend in a call with the maintainer.
