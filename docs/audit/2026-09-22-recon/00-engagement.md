<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Engagement — reconnaissance pass

**Slug:** `2026-09-22-recon`
**Frozen commit:** `ab1a85e8e112dd656fadfc01904e518bdfebbb79`
**Working tree at freeze:** clean (`git status --porcelain` empty)
**Date:** 2026-09-22

## Scope

A **phase 1-2 engagement only**: familiarisation and claim verification, plus an
independent threat model. The deep-review workstreams (crypto, protocol,
implementation, clients, backend) do **not** run in this engagement.

### In scope

- **The whole repository, for the purpose of mapping it.** A surface map that stops
  at the reviewed surface is not a map; recon establishes what exists before anyone
  decides what to review.
- **The vendor's own documented claims**, wherever they are cheap to check against
  the tree: `docs/security/external-review-scope.md`, `CLAUDE.md`, module-level
  documentation, the invariant index.
- **An independent threat model**, written before reading
  `docs/security/threat-model.md`, followed by the delta.

### Out of scope

- Every deep review. This engagement produces the baseline they would consume; it
  does not pre-empt their judgements, and a deep question noticed here is recorded
  as an open question rather than pursued.
- Anything requiring a deployed environment, a device, or a platform toolchain.

### Why this shape

This is the first run of the `audit-*` team. A recon pass is the cheapest phase and
the one whose output is checkable line by line against the repository, so it answers
two questions at once: what does the project actually expose, and do these skills
work on real code.

## Rules of engagement

The four rules in
[`.claude/skills/audit-lead/methodology.md`](../../../.claude/skills/audit-lead/methodology.md):

1. Code first; documents are claims under test, never evidence.
2. Evidence is a `path:line` read this session, or it is a question, not a finding.
3. Measured or absent — no count that did not come out of a command.
4. Read-only. Nothing written outside this directory; no commits; no deploy or
   restart commands. No production systems exist to touch.

## Workstreams

| Workstream | Skill | Deliverable | Status |
|---|---|---|---|
| Familiarisation and claim verification | `audit-recon` | `01-surface.md` | dispatched |
| Independent threat model | `audit-recon` | `02-threat-model.md` | dispatched |

Findings file under `findings/` with the `LAK-RC-NN` prefix.

## Progress

- 2026-09-22 — engagement opened, commit frozen, `audit-recon` dispatched.
