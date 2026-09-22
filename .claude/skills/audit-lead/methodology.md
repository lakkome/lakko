# External audit methodology

The phase model every Lakko audit engagement follows. It mirrors what an
independent review firm does: scope, familiarise, model the threat *yourself*,
review in parallel workstreams, measure rather than assume, try to kill your own
findings, report, retest.

Read this before any workstream. The lead owns phases 0, 6 and 7; the specialists
own 1-5.

---

## The four rules that make this an audit and not a walkthrough

**1. Code first, documents as claims.**
Lakko's documentation is unusually rich — 10k lines of security docs, 18k lines of
ADRs. That is a hazard, not a shortcut. A document is a *claim under test*, never
evidence. Read the source, build your own model, and only then open the document to
see whether it describes what you read. Its own scope brief says so in §7: *"Trust
the code over its comment, and tell us when you find one we missed."*
A finding may never rest on a document alone.

**2. Evidence standard: `path:line`, read this session.**
Every claim in every workstream note cites a file and line you actually opened. No
claim from memory of the codebase, from CLAUDE.md, from an ADR table, or from a
previous session. If you cannot trace a mechanism through code, it is a *question
for the vendor*, not a finding — file it as such.

**3. Measure or stay silent.**
Never state a count, a timing, a pass rate, a coverage figure or a size you did not
produce by running a command this session. "I did not measure this" is an acceptable
sentence in an audit report. A fabricated number is the one thing that destroys the
report's value entirely. This restates the project's own hard rule in CLAUDE.md and
it binds auditors twice over.

**4. Read-only. You review; you do not repair.**
No auditor modifies source, tests, configuration or documentation outside the
engagement directory. No `git commit`, no `git checkout`, no dependency changes, no
running of deploy commands (`just deploy*`, `just restart*`, anything touching the
VPS or coturn). Recommendations go in the report; the maintainer decides and
implements. Writing a *proof-of-concept test* is allowed only as an untracked file
under the engagement directory, never inside `crates/` or `apps/`.

---

## Working to a budget — the rule every workstream follows

A real reviewer has a fixed number of days and a surface larger than those days. So
do you. The failure mode is not running out; it is **running out invisibly** — going
wide, going shallow everywhere, and producing a note whose silence reads as coverage.

Every workstream therefore carries a **priority ladder**: its areas ordered by where
the risk is, most-likely-to-matter first. Work it in order and obey three rules.

**1. Depth before breadth.** Finish a rung before starting the next. Two rungs read
properly are worth more than seven skimmed, because a skim finds only what is
obvious, and what is obvious has usually already been found by the people who wrote it.

**2. Stop while you can still write.** When you judge you have room for about one
more rung, take no new rung: spend what is left on the note. An unwritten conclusion
helps nobody, and a workstream that stops mid-investigation with a good note is a
useful workstream, while one that investigated further and reported badly is not.

**3. "Found nothing" and "did not reach" are different sentences and may never be
written as one.** For every rung you did not take, say so explicitly, and say what
you would have done. A rung you skipped is *unknown*, not *clear*, and the report's
coverage statement is assembled from exactly these sentences. If you leave the
distinction to the reader, they will read silence as safety, which is the one
inference an audit must never invite.

When the lead asks "was that a finding count or an attention count?", the answer must
already be in your note.

## Retest mode — when the engagement is a re-check of earlier findings

A retest is not a smaller audit. You are handed specific findings from a previous
engagement and a new frozen commit, and you answer one question per finding.

- **Do not re-review the surface.** New findings outside the retest scope are welcome
  but are not the job; note them briefly and stay on the list.
- **Re-derive the original mechanism against the changed code.** Read the code as it
  is now and walk the original steps against it. Never verify a fix by reading the
  commit message, the ADR, or the vendor's account of what they changed — those
  describe the intent, and the intent was not what was broken.
- **Verdict per finding:** `Fixed` · `Partially fixed` (say which part, and the
  residual severity) · `Not fixed` · `Risk accepted by vendor` (quote the acceptance).
- **A fix that relocates the defect is `Not fixed`**, and the note says where it went.
  So is a fix that closes the reported instance and leaves the class open — the
  project has one of those in its own history and it is the more instructive kind.
- **Raise findings against the fixes themselves.** A change made under time pressure
  to close an audit finding is exactly the kind that introduces the next one.

## Phase 0 — Engagement setup (lead)

Produces `00-engagement.md`.

- **Freeze the tree.** Record `git rev-parse HEAD`, `git status --porcelain` and the
  date. Everything in the report refers to that commit. If the tree is dirty, say so
  and list the dirty paths — a review of an uncommitted working tree is a valid
  engagement but the report must state it.
- **Fix the scope.** In-scope components, explicitly out-of-scope components, and the
  reason for each exclusion. Default anchor: `docs/security/external-review-scope.md`
  §2. The lead may widen or narrow it, but the delta from that brief is recorded.
- **State the rules of engagement** (the four rules above, plus: no production
  systems are touched because none exist).
- **Name the workstreams** that will run and who (which skill) owns each.
- **Get the maintainer's sign-off on scope before the specialists start.** Scope is
  the client's decision, not the auditor's.

## Phase 1 — Familiarisation and claim verification (`audit-recon`)

Produces `01-surface.md`.

Build the map independently, then test what the vendor says about it:

- Component inventory: crates, services, clients — measured sizes, not quoted ones.
- **Attack-surface inventory**: every parser reachable from an untrusted source,
  every network endpoint, every FFI boundary, every persistence surface, every
  place where attacker-controlled bytes meet a length or an index.
- Trust boundaries and data flows: who holds what key, what crosses which boundary.
- **Claim verification**: take the vendor's stated facts and check them. Test counts.
  CI trigger reality. Which services actually have a deploy path. Which security
  flags default off. Which module comments describe a state the code has left. The
  scope brief hands you a list of things it admits it cannot guarantee about itself —
  start there and do not stop there.

## Phase 2 — Independent threat model (`audit-recon`)

Produces `02-threat-model.md`.

Derive it yourself before reading `docs/security/threat-model.md`:
assets → adversaries and their capabilities → attack goals → trust boundaries.
Then open the vendor's model and produce the **delta**: what they claim that you
would not, what you would claim that they do not, and — the valuable column —
**what neither of you claims**, i.e. properties a user would reasonably assume and
nobody has committed to.

## Phase 3 — Review workstreams (parallel specialists)

Each produces one workstream note and zero or more findings.

| Workstream | Skill | Note |
|---|---|---|
| Cryptographic design and primitives | `audit-crypto` | `10-crypto.md` |
| Protocol and group state machine | `audit-protocol` | `11-protocol.md` |
| Implementation (Rust) | `audit-impl` | `12-implementation.md` |
| Three-client convergence | `audit-clients` | `13-clients.md` |
| Server and zero-trust posture | `audit-backend` | `14-backend.md` |

They run in parallel and do not read each other's notes while working — convergent
independent findings are a signal, and cross-reading destroys it. Overlap at the
seams is expected and is resolved in phase 5, not avoided in phase 3.

## Phase 4 — Dynamic analysis (`audit-impl`, its tooling half)

Produces `15-dynamic.md`. Runs concurrently with phase 3.

Everything that yields a *measured* number: the test suite, fuzz targets against a
real budget, dependency advisories, lints, sanitizers, reproducible-build check,
secret scanning. Reports what ran, what it found, and — just as important — what
could not run here and why.

## Phase 5 — Consolidation and adversarial verification (`audit-verify`)

Produces `20-verification.md` and the final finding set.

Every candidate finding is attacked: try to prove it wrong. Read the code path
again looking for the guard that makes it unreachable. A finding that survives is
`CONFIRMED`; one that cannot be settled without running something unavailable is
`PLAUSIBLE`; one that dies is moved to the non-findings appendix **with the reason
it died**, because that is information the vendor paid for too. Severity is
calibrated here against `severity.md`, not by the finder.

## Phase 6 — Report (lead)

Produces `REPORT.md` from `report-template.md`. Assembled, not concatenated: the
executive summary must be readable by someone who will never open a finding.

## Phase 7 — Retest (lead)

Produces `RETEST.md`, on a later run, against a new frozen commit. Per finding:
`Fixed` / `Partially fixed` / `Not fixed` / `Risk accepted by vendor`, each with the
evidence that settled it. A fix is verified by reading the fix and re-running the
original reasoning — not by reading the commit message.
