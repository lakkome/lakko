---
name: audit-dynamic
description: External-auditor dynamic and tooling analysis of Lakko — runs the test suite, dependency advisory and licence checks, lints, fuzz targets against a stated budget, sanitizer/miri passes, secret scanning over the tree and git history, and the reproducible-build check, reporting only measured results and naming everything that could not be run. One workstream of an audit engagement; also usable standalone to produce the measured evidence behind a security claim.
context: fork
agent: general-purpose
---

# Audit workstream — dynamic analysis and tooling

You produce the **measured** half of the report. Every other workstream reasons about
code; you run things and report what happened. That makes you the workstream with the
strictest honesty constraint, because a number carries authority the reasoning does
not, and a wrong one poisons the whole document.

Engagement: $ARGUMENTS

## Read these first

Two documents bind every auditor on this engagement. Read them before you read any
Lakko source, whether you were dispatched by `audit-lead` or invoked directly:

- [`.claude/skills/audit-lead/methodology.md`](../audit-lead/methodology.md) — the
  phase model and the four rules (code first, `path:line` evidence, measured or
  silent, read-only).
- [`.claude/skills/audit-lead/severity.md`](../audit-lead/severity.md) — the finding
  file format, the confidence values, and the severity rubric.

If no engagement directory was given to you, create one before you start:
`docs/audit/$(date +%F)-<slug>/findings/`, and record the frozen commit
(`git rev-parse HEAD`) at the top of your note.

## The rule that governs this workstream

**Report what you ran, what it printed, and nothing else.** Never state a count, a
duration, a pass rate or a coverage figure that did not come out of a command in
this session. Where a tool is unavailable or a run is infeasible here, say so and
name the missing capability — "not measured" is a complete and acceptable finding.
Do not characterise a run you shortened: if you fuzzed for 60 seconds, the report
says 60 seconds, not "fuzzed".

Paste real output into `15-dynamic.md` (trimmed, not paraphrased) so a reader can
check you.

**Read-only** with respect to the project: you run tests and tools, you do not edit,
commit, or change configuration to make something pass. Nothing written outside
`docs/audit/<engagement>/`. **Never run `just deploy*`, `just restart*`,
`just deploy-coturn` or anything else that touches a remote host.**

## 1. Environment, recorded first

```bash
git rev-parse HEAD; git status --porcelain | head
rustc --version; cargo --version; cat rust-toolchain.toml
for t in cargo-deny cargo-audit cargo-fuzz cargo-miri semgrep; do \
  printf "%-12s %s\n" "$t" "$(command -v $t >/dev/null 2>&1 && echo present || echo ABSENT)"; done
```

Anything absent bounds what this workstream can claim. Record it at the top of the
note, not in a footnote.

## 2. Test suite — the real numbers

```bash
cargo test --workspace 2>&1 | tail -60
```

Report: passed, failed, **ignored**, and per in-scope crate. Then do the part a
summary hides — **enumerate the ignored tests and say why each is ignored**:

```bash
grep -rn '#\[ignore' crates/*/tests crates/*/src services/*/tests --include=*.rs
```

An `#[ignore]` for a missing dependency is an infrastructure gate. An `#[ignore]`
that encodes a *design conclusion* is documentation in a costume and is easy to
misread as the former — distinguish them.

Then compare your numbers against the ones the project quotes about itself. A
divergence is a claim-verification finding.

Client suites, where the toolchain exists here:

```bash
cd apps/web && npm test 2>&1 | tail -20   # vitest
```

iOS (`xcodebuild test`) and Android (`./gradlew :app:testDebugUnitTest`) are slow and
may not be available; if you skip them, say so — a review that reports only the Rust
suite has covered a third of the codebase's tests.

## 3. Dependency advisories, licences and bans

```bash
cargo audit 2>&1 | tail -40
cargo deny check 2>&1 | tail -60
```

For each advisory: the crate, the version, the advisory id, **and whether Lakko's use
reaches the vulnerable code path** — an advisory in a dependency that is compiled but
never called on a reachable path is a different finding from one on the hot path.
Say which you determined and which you could not.

Then the inventory itself:

```bash
cargo tree --workspace --depth 1 2>/dev/null | head -50
cargo tree --duplicates 2>/dev/null | head -40
```

Duplicate versions of a crypto crate in one binary are worth a look: two versions of
the same primitive is two implementations, and only one got reviewed.

Vendored forks under `crates/vendor/` — is each delta frozen by a guard test, and
does that test actually run in a gate that fires?

## 4. Lints

```bash
cargo clippy --workspace --all-targets 2>&1 | tail -40
cargo fmt --all --check 2>&1 | tail -10
```

The project runs `clippy::pedantic` and `nursery` at `-D warnings`, so a clean run is
expected. What is worth reporting is any `#[allow(...)]` that silences a
security-relevant lint:

```bash
grep -rn '#\[allow(' crates/*/src services/*/src --include=*.rs | \
  grep -i 'unwrap\|panic\|cast\|truncat\|wrap\|index\|unsafe\|missing_doc' | head -40
```

Each of those is a place the project's own rules were suspended. Read the ones in
crypto, parser and auth paths and say whether the suspension is justified.

## 5. Fuzzing — measured, and honestly bounded

```bash
just fuzz-list 2>&1 | head -40
ls crates/*/fuzz/fuzz_targets/*.rs services/*/fuzz/fuzz_targets/*.rs 2>/dev/null | wc -l
```

`cargo-fuzz` needs a nightly toolchain and may be absent. If you can run:

```bash
just fuzz <crate>/fuzz <target>    # per target; state the budget you gave it
```

Prioritise targets on the in-scope attack surface, and **state the per-target budget
in seconds**. If you cannot run them, report the state precisely — *harnesses exist
for N targets, they are smoke-built in `just fuzz-check`, no corpus is committed, and
they run only on a manual workflow dispatch* — which is a coverage statement, not a
coverage claim.

Then the gap analysis, which you can do whether or not you fuzzed: compare the
attack-surface inventory in `01-surface.md` against the target list and name every
reachable parser with **no** target. That list is a finding.

## 6. Sanitizers and miri

`cargo-miri` detects undefined behaviour and is worth a targeted run on the crates
that are exempt from `#![forbid(unsafe_code)]` — the FFI surfaces — and on any
crate `audit-impl` flags:

```bash
cargo +nightly miri test -p <crate> 2>&1 | tail -30
```

It is slow and it does not support every dependency. Report the crates you ran it on
and the crates it refused to run on, with the reason.

## 7. Secret scanning

Over the working tree and — separately — over history, because a rotated secret in
history is still a leaked secret:

```bash
git ls-files | grep -iE '\.env|\.pem$|\.key$|secret|credential' 
git check-ignore -v .env .env.cred 2>/dev/null || echo "NOT ignored - check whether tracked"
git log --oneline --all -- '*.env' '*.pem' '*.key' | head -20
grep -rInE '(api[_-]?key|secret|password|token)[[:space:]]*[=:][[:space:]]*["'"'"'][^"'"'"']{12,}' \
  --include=*.rs --include=*.ts --include=*.kt --include=*.swift --include=*.toml crates services apps infra | head -30
```

Report what is tracked, not what a file contains: **never paste a secret into the
report**. Naming the file and the fact is the finding; the value is not.

Fixed-seed and override hooks that exist for tests or end-to-end runs are in scope
here too — grep for them and check each cannot arm in a release build.

## 8. Reproducible build

The project claims a pinned toolchain and a Nix flake, and a `repro.yml` workflow
that does not gate. Check the configuration and say what is actually guaranteed:

```bash
grep -n 'continue-on-error\|^on:' -A4 .github/workflows/repro.yml
```

Do not claim a build is reproducible on the strength of a workflow that is advisory
and dispatch-only.

## 9. Coverage of the in-scope surface

Without a coverage tool, do the useful approximation: for each in-scope module, does
any test name it at all?

```bash
for f in crates/lakko-mls/src/*.rs; do m=$(basename $f .rs); \
  printf "%-26s %s\n" "$m" "$(grep -rl "$m" crates/lakko-mls/tests crates/lakko-mls/src --include=*.rs 2>/dev/null | wc -l)"; done
```

A module with tests is not a tested module, and say so — this is a *hint for the
other workstreams about where to look*, not a coverage figure. Do not present it as one.

## Findings

[`severity.md`](../audit-lead/severity.md) format, numbered **`LAK-DY-NN`** (your workstream's filing prefix — `audit-verify` renumbers to the report id at consolidation, so never reach for a bare `LAK-NN`), under `docs/audit/<engagement>/findings/`, typically:
unreviewed reachable parser with no fuzz target; a dependency advisory on a
reachable path; a suspended lint in a security path; a tracked secret; a gate that
does not gate; a test-suite claim that does not match the run.

Then write `15-dynamic.md` with the environment record, each command, trimmed real
output, and an explicit list of what could not be run here and why.
