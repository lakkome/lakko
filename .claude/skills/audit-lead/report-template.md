# Final report skeleton

`docs/audit/<engagement>/REPORT.md`. Assembled by the lead in phase 6. It is a
deliverable for a reader who will never open a workstream note — assemble it, do not
concatenate the notes into it.

Carries the SPDX header like every Markdown file in this repository.

---

```markdown
<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — security audit report

**Engagement:** <slug>
**Commit reviewed:** <full SHA>  (<dirty|clean> working tree)
**Dates:** <start> - <end>
**Scope:** one sentence. Full scope in §2.

---

## 1. Executive summary

Four to eight paragraphs, no jargon that a non-cryptographer cannot follow, no
finding IDs in the first one. It answers, in this order:

- What was reviewed and what was deliberately not.
- The single most important thing found. If nothing severe was found, say that
  plainly and immediately — an audit that finds little is a result, not a failure,
  and burying it under hedging is dishonest.
- The shape of the findings as a group: are they one root cause wearing several
  faces, or independent? Which layer do they cluster in?
- What the reviewer's confidence rests on, and where it is thin.
- The recommendation: is the release gate this engagement serves safe to close?

## 2. Scope

### 2.1 In scope
Components, with the reason each is in.

### 2.2 Out of scope
Components excluded, with the reason each is out. An exclusion with no reason is a
hole in the report.

### 2.3 Rules of engagement
Read-only review, no production systems (none exist), frozen commit, the evidence
standard.

## 3. Methodology

The phases actually executed, with the workstreams that ran and the tools used.
Name what was run and what it measured — this is where a reader calibrates how much
the rest of the document is worth.

## 4. Findings summary

| ID | Title | Severity | Confidence | Component | Status |
|---|---|---|---|---|---|

Ordered by severity, then by ID. Counts by severity underneath.

## 5. Findings

Each finding in full, in the `severity.md` format, severity-ordered.

## 6. Hardening observations

Things that are not defects but would raise the floor. Numbered `H-01..`, each one
sentence of what and one of why.

## 7. Open questions for the vendor

Where reviewer attention snagged without resolving. Each is a question, phrased so
it can be answered — not a vague unease. These are as valuable as findings when the
answer is "we had not thought about that".

## 8. Positive observations

What is genuinely well built, specifically and with evidence. A report with none is
not being rigorous; it is being lazy. This section is also how a reader calibrates
the critical sections — a reviewer who found nothing good did not look.

## 9. Coverage statement

**Mandatory, and the section a serious reader turns to first.**

| Area | Depth | Basis |
|---|---|---|

Depth is one of: `line-by-line` · `targeted` (driven by a specific question) ·
`survey` (structure and entry points only) · `not reviewed`.
Then, in prose:

- What could not be exercised here and why (no deployed environment, no device, no
  platform toolchain, no fuzzing budget).
- Where the review depended on the vendor's own documentation rather than on code,
  if anywhere.
- Which of the vendor's ranked questions (`external-review-scope.md` §6) were
  answered, which were partially answered, and which were not reached.
- What a follow-up engagement should cover first.

## 10. Appendices

- **A. Claim verification log** — vendor statements checked, with verdicts.
- **B. Non-findings** — candidates that died in verification, each with the reason.
  A reviewer who reports only survivors hides their own error rate.
- **C. Measured data** — command outputs behind every number in the report.
- **D. Files reviewed** — the inventory, with the depth from §9.
```

---

## Retest report

`RETEST.md`, written in phase 7 against a new frozen commit.

```markdown
# Lakko — audit retest

**Original engagement:** <slug> at <SHA>
**Retest commit:** <SHA>
**Date:**

## Summary
Counts by verdict, and whether any fix introduced a new issue.

## Per-finding verdict

### LAK-01 — <title>  ->  Fixed
What changed (`path:line`), and the reasoning re-run against it. A fix is verified
by reading the code and re-deriving the original mechanism against it — never by
reading the commit message or the vendor's account of the fix.

### LAK-02 — <title>  ->  Partially fixed
Which part closed, which remains, and the residual severity.

## New issues introduced by fixes
Findings raised against the fixes themselves, in the standard format.
```
