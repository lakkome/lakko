<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Audit engagements

Where independent review engagements are recorded. One directory per engagement,
named `<YYYY-MM-DD>-<slug>`.

These are **external-auditor** reviews: a surface at a frozen commit, reviewed from
the code by a team that treats this repository's own documentation as claims under
test rather than as evidence. They are not the same thing as
[`lakko-security`](../../.claude/skills/lakko-security/SKILL.md), which is the
in-house review of a single change against the invariant index. Both are useful and
they catch different classes; neither substitutes for the other, and neither
substitutes for the real external cryptographic review that
[G-39](../security/known-gaps.md) tracks as a release gate.

## Running one

The team lives in `.claude/skills/audit-*`. The entry point is `audit-lead`, which
scopes the engagement, dispatches the workstreams, runs the verification gate and
assembles the report. Its reference documents — the phase model, the severity rubric
and the report skeleton — are in
[`.claude/skills/audit-lead/`](../../.claude/skills/audit-lead/).

The natural scope anchor is
[`../security/external-review-scope.md`](../security/external-review-scope.md), the
brief written for the G-39 engagement.

## Engagement layout

```
docs/audit/<YYYY-MM-DD>-<slug>/
  00-engagement.md      frozen commit, scope in/out with reasons, rules, workstreams
  01-surface.md         attack-surface map and the vendor-claim verification log
  02-threat-model.md    the reviewers' own model, and the delta from the vendor's
  10-crypto.md          workstream notes
  11-protocol.md
  12-implementation.md
  13-clients.md
  14-backend.md
  15-dynamic.md         measured runs, with real output (audit-impl's second half)
  findings/LAK-NN.md    one file per finding
  20-verification.md    kill attempts, severity calibration, non-findings
  REPORT.md             the deliverable
  RETEST.md             written later, against a new frozen commit
```

Every file carries the SPDX header, as everything under `docs/` does.

## Reading a report

Four sections carry most of the signal, and they are not the finding list:

- **§9 Coverage statement** — what was actually reviewed and how deeply. A report
  without it is a list of opinions.
- **Appendix B, non-findings** — candidates that were killed in verification, with
  the reason. It is how you calibrate the reviewers' error rate.
- **§7 Open questions** — where attention snagged without resolving. Often the most
  useful section when the answer is "we had not thought about that".
- **§8 Positive observations** — a report with none did not look.

## Index

_No engagement has been run yet._

| Engagement | Commit | Scope | Findings | Report |
|---|---|---|---|---|
