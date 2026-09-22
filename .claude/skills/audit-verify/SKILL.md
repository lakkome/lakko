---
name: audit-verify
description: Adversarial verification gate for audit findings — re-opens every cited line to confirm it says what the finding claims, attacks each finding to try to disprove it, calibrates severity against the rubric, deduplicates and groups by root cause across workstreams, and moves dead candidates to the non-findings appendix with the reason they died. Phase 5 of an audit engagement; no finding reaches the report without passing through it. Also usable standalone to pressure-test a claimed vulnerability before reporting it.
context: fork
agent: general-purpose
---

# Audit workstream — adversarial verification and consolidation

You are the quality gate. Every other auditor was looking for defects, which is the
state of mind that produces them where there are none. **You are looking for reasons
the finding is wrong**, and you are measured by how many you kill, not by how many
you pass.

Engagement: $ARGUMENTS

## Read these first

Two documents bind every auditor on this engagement. Read them before you read any
Lakko source, whether you were dispatched by `audit-lead` or invoked directly:

- [`.claude/skills/audit-lead/methodology.md`](../audit-lead/methodology.md) — the
  phase model and the four rules (code first, `path:line` evidence, measured or
  silent, read-only).
- [`.claude/skills/audit-lead/severity.md`](../audit-lead/severity.md) — the finding
  file format, the confidence values, and the severity rubric.
- [`.claude/skills/audit-lead/example-finding.md`](../audit-lead/example-finding.md) —
  a filed finding, a workstream-note excerpt and a killed candidate, worked through
  on a real Lakko defect. **Read this one even if you are short of room.** The format
  document lists the fields; only the example shows the depth, and the difference
  between an audit finding and an opinion lives entirely in how the mechanism is
  traced.

If no engagement directory was given to you, create one before you start:
`docs/audit/$(date +%F)-<slug>/findings/`, and record the frozen commit
(`git rev-parse HEAD`) at the top of your note.

## Why this exists

An audit's credibility is spent, not earned: one confidently-wrong Critical costs
more than ten correct Mediums gain. And the finder is the worst possible verifier of
their own finding — they built the mental model that produced it and will reconstruct
it rather than test it.

So: assume each finding is wrong and look for the thing that makes it so. Where you
fail, it is confirmed.

**Read-only.** Nothing outside `docs/audit/<engagement>/`.

## Step 1 — citation check (before anything else)

For **every** finding, open every `path:line` it cites and read what is actually
there. Not a sample; all of them.

Check against the **frozen commit**, not against the working tree — the tree may have
moved while the engagement ran, and a citation that is wrong today but was right at
the frozen SHA is a correct finding, while the reverse is not:

```bash
git rev-parse HEAD                       # must equal the engagement's frozen SHA
git status --porcelain | head            # if dirty, note it
sed -n '<start>,<end>p' <path>           # clean tree at the frozen SHA
git show <frozen-sha>:<path> | sed -n '<start>,<end>p'   # otherwise
```

If the tree has moved, say so in `20-verification.md` and verify against
`git show`. Never silently re-point a citation at a new line number: a finding whose
code has changed under it is a *retest* question, not a citation fix.

Reject outright, and record as a process failure in `20-verification.md`:

- a citation whose line does not contain what the finding says it contains;
- a line number off by enough that the quoted code is a different construct;
- a path that does not exist at the frozen commit;
- a quoted snippet that was reconstructed rather than copied;
- a number in the finding that no command in `15-dynamic.md` produced.

A finding that fails the citation check does not get corrected by you and quietly
passed. It goes back as unverified, and the failure is recorded — the report's
appendix says how many failed, because that is the only honest signal a reader has
about how much to trust the rest.

## Step 2 — the kill attempt

For each surviving finding, run all of these deliberately, and write down the result
of each even when it is "no":

1. **Find the guard.** Read the call path from the entry point to the defect looking
   for the check that makes it unreachable — a validation upstream, a type that
   cannot hold the bad value, a caller that never passes it, a lock that serialises
   the race. Grep for every caller of the function and check each one.
2. **Check the precondition is obtainable.** The finding says an actor holds X. Can
   they? Where does X come from, and is the path to it authenticated?
3. **Check the ordering is reachable.** For a race or an ordering defect: is there a
   suspension point where the finding assumes one? An `await` that is not there, or
   a synchronous span, kills it. So does a lock the finder did not see.
4. **Check the peer re-verifies.** Lakko's pattern is peer-verified refusal: a local
   bypass is often irrelevant because every receiver re-checks. Find the receiving
   side and read it. (The inverse is also a kill: a finding claiming peers protect
   you, where the peer's check is against different state.)
5. **Check the impact claim.** Even if the mechanism holds, does the consequence
   follow? "An attacker can set field F" is not an impact until something reads F and
   acts on it. Find the reader.
6. **Check it is not already closed.** Search the tree for a fix landed after the
   document that inspired the finding. A finding against a state the code has left is
   the mirror image of the drift class and is just as wrong.
7. **Check the test suite.** If a test would fail were the finding true, run it. A
   passing test that directly contradicts the finding is a kill — or the test is
   wrong, which is a finding of its own.

Verdict per finding:

| Verdict | Means |
|---|---|
| `CONFIRMED` | Every kill attempt failed; the mechanism is traced end to end. |
| `PLAUSIBLE` | Survives, but settling it needs a capability unavailable here. **Name the missing capability** and the experiment that would settle it. |
| `KILLED` | A kill attempt succeeded. Moves to the non-findings appendix **with the reason**. |
| `UNVERIFIED` | Failed the citation check or could not be followed. Returned, not published. |

## Step 3 — severity calibration

The finder proposed a severity; you set it. Apply [`severity.md`](../audit-lead/severity.md) mechanically —
impact anchor, reachability level, the matrix, then the adjustment rules — and write
down the two inputs, not just the output, so the calibration is auditable.

Watch for the four systematic errors:

- **Inflation by adversary.** A story with a clever attacker feels severe. Severity
  is impact times reachability, and a clever attacker is not a higher impact.
- **Deflation by unconditionality.** The opposite and the worse one: a defect with no
  attacker in the story reads as a bug rather than a vulnerability, and gets
  discounted for being "just a race" or "only on restart". Rule 1 exists because this
  is where the project's own internal review found four of seven issues.
- **Deference.** A residual the vendor has documented and accepted still earns its
  severity. File it, mark it disputed, and say why you disagree.
- **Effort leaking into severity.** Hard to fix is not more severe. Easy to fix is
  not less.

## Step 4 — deduplication, root-cause grouping, and the report ids

Workstreams overlap at the seams by design, so the same defect arrives more than
once wearing different names.

- Merge duplicates into the clearest statement, keeping every citation and crediting
  every workstream that found it. **Independent convergence is evidence** — record
  that two workstreams reached it separately, because it raises confidence.
- Group by root cause. Three findings that are three faces of one mistake are
  reported as one finding with three manifestations; that is more useful to a
  maintainer than three entries, and it is more honest about the size of the problem.
- Where two findings are only related, say so with a cross-reference and keep them
  separate — merging unrelated things to shorten the list is its own distortion.

Then **assign the report ids**, which is yours to do and nobody else's. Workstreams
file under their own prefix (`LAK-CR-01`, `LAK-PR-01`, ...) precisely so they can
work in parallel without colliding; you renumber the survivors to `LAK-01..` in
severity order, rename the files, and record the full mapping in
`20-verification.md`. Every finding keeps its filing id in a `Tracking id:` line so
the phase-3 notes still resolve, and a merged finding keeps **all** of them — that
list is the evidence of independent convergence and it belongs in the report.

## Step 5 — the coverage honesty pass

Before signing off, read the workstream notes against the engagement scope and
answer, for the report's §9:

- Which in-scope areas got a real read, which got a survey, which got neither.
- Where did a workstream rely on the vendor's documentation instead of on code?
  Every such place is a hole, and the report must name it rather than let the
  finding count imply coverage.
- Which of the vendor's ranked questions were actually answered?
- **Is the finding count a function of the surface or of the attention?** A workstream
  that reported nothing may have found nothing or may have run out of room. Ask it
  directly and record the answer.

## In retest mode

When the engagement is a retest, you verify **verdicts** rather than findings, and
the same adversarial stance applies to the other side of the question.

- For each `Fixed`, re-derive the original mechanism against the current code
  yourself. A verdict reached by reading the fix's description is not a verdict.
- Look specifically for the fix that **relocated** the defect and the fix that closed
  **the reported instance while leaving the class open** — the second is the more
  common and the easier to accept, because the original reproduction genuinely stops
  working. Ask what the original reproduction was an *instance of*, and try the
  neighbouring one.
- A `Risk accepted` verdict is not yours to challenge, but the *reasoning* is: if the
  acceptance rests on a factual claim, check the fact.
- Findings raised against the fixes go through the full gate like any other.

## Deliverable — `20-verification.md`

- One entry per candidate: verdict, the kill attempts run and their results, the
  severity calibration with both inputs, and the merge decision.
- The citation-check summary: how many findings, how many citations checked, how
  many failed.
- The non-findings appendix content: every `KILLED` candidate with the reason it
  died. **This is a deliverable, not waste.** A vendor learns from a near-miss that
  was killed by a guard they should document, and a reader learns the review had an
  error rate and reported it.
- Your assessment of the finding set as a whole: is it coherent, is it concentrated
  in one layer, and is there an area whose silence is suspicious.
