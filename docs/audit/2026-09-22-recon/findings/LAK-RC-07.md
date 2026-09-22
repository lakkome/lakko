<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# LAK-RC-07 — No automated CI gate runs tests, lints, formatting, licensing or dependency checks; the only automatic workflow checks documentation drift

- **Severity:** Medium (finder's proposal; `audit-verify` calibrates)
- **Confidence:** CONFIRMED
- **Workstream:** recon
- **Tracking id:** LAK-RC-07
- **Status:** Open
- **Component:** `.github/workflows/ci.yml:7-11`, and every other file in
  `.github/workflows/`
- **Invariants touched:** none directly; this is the control that would catch
  violations of all of them
- **Vendor claim contradicted:** `CLAUDE.md`: *"**Every file** carries the
  `SPDX-License-Identifier` header. … No exceptions. CI blocks otherwise."* and
  *"`clippy::all`, `clippy::pedantic`, `clippy::nursery` — all warnings on.
  `-D warnings` in CI."* Conceded by `docs/security/external-review-scope.md` §7.

## Preconditions

None. It is the standing configuration.

## Mechanism

Trigger blocks for all 11 workflows, extracted this session:

| Workflow | Triggers |
|---|---|
| `spec-drift.yml` | `pull_request: branches: [main]` |
| `ci.yml` | `workflow_dispatch` only |
| `fuzz-quick.yml` | `workflow_dispatch` only |
| `censorship-lab.yml` | `workflow_dispatch` only |
| `deploy-web.yml` | `workflow_dispatch` only |
| `g3-4-mobile.yml` | `workflow_dispatch` only |
| `recovery-e2e.yml` | `workflow_dispatch` only |
| `repro.yml` | `workflow_dispatch` only |
| `sframe-bridge.yml` | `workflow_dispatch` only |
| `sframe-interop.yml` | `workflow_dispatch` only |
| `sfu-e2e.yml` | `workflow_dispatch` only |

`ci.yml:7-11` is explicit — the automatic triggers are present and commented out:

```yaml
on:
  # push:
  #   branches: [main]
  # pull_request:
  #   branches: [main]
  workflow_dispatch: {}
```

`fuzz-quick.yml` carries a reason (`:2-9`): *"Quota-conservation mode (May 2026).
Auto-triggers … are disabled — the Free-tier 2,000-minute budget gets eaten by the
fuzz matrix in under a week of nightly cron."* `ci.yml` carries no such note.

So on any push or pull request, what runs is `spec-drift.yml` — the check that
`<module>.sc.md` specs have been updated alongside their modules. Nothing compiles,
nothing tests, nothing lints, nothing checks SPDX headers, nothing checks
dependency advisories or licences.

## Reproduction

```
for f in .github/workflows/*.yml; do printf '%-24s' "$(basename $f)"; \
  sed -n '/^on:/,/^[a-z]/p' "$f" | grep -E '^  [a-z_]+:'; done
sed -n '5,12p' .github/workflows/ci.yml
```

## Impact

Every quality and security control the project documents as enforced is, in
practice, a convention observed by whoever is at the keyboard. In particular:

- `cargo test --workspace` — 2,771 tests, and none of them gate a merge.
- `cargo clippy -- -D warnings` — the pedantic/nursery lint set `CLAUDE.md`
  describes as mandatory.
- `reuse lint` — the SPDX requirement described as blocking.
- `cargo deny check` — the supply-chain control. Its absence is the one that
  compounds worst: a new transitive dependency with a known advisory enters the
  tree with no signal at all, and the threat model's coerced-maintainer row rests
  on a release-integrity story (`repro.yml`) that likewise never runs.
- `cargo fmt --check`.

This does not mean any of those currently fail — the workspace test run in
`01-surface.md` §4.4 passed 2,771/2,771 on this machine at this commit, so the
present state is good. It means **nothing preserves that state**, and the
relationship between "a reviewer read this commit" and "this artefact reached a
user" is unmediated. That is the property an external review most needs to hold,
because a review is a statement about a specific tree.

It compounds with **LAK-RC-08** (23 database-backed security tests pass without
executing when no test DB is configured): even on a manual `ci.yml` dispatch,
those tests report success on a runner that has no MariaDB.

## Recommendation

**Re-enable `push` and `pull_request` on a reduced `ci.yml`.** The quota argument
is real and is stated honestly for the fuzz matrix, where it is a good argument —
a full fuzz matrix on every push is indefensible on 2,000 free minutes. It is a
much weaker argument for `cargo check`, `cargo fmt --check`, `clippy` and
`reuse lint`, which are cheap and catch the class of regression that is most
expensive to find later. A tiered split — a fast always-on gate (fmt, clippy,
check, reuse, deny) on every PR, and the expensive matrix on dispatch or a weekly
cron — costs a fraction of the budget and restores the documented posture.

**Failing that, correct the documents.** `CLAUDE.md`'s "CI blocks otherwise" and
"`-D warnings` in CI" should say that these are local pre-commit obligations, not
enforced gates, and the pre-commit checklist should be framed as what it is. The
scope brief §7 already does this; the developer-facing document does not.

`cargo deny` in particular should run on a schedule regardless, since advisory
databases change without the tree changing — a commit that was clean in May is not
necessarily clean in September, and nothing here would notice.

## Notes

- Conceded in `docs/security/external-review-scope.md` §7 (*"ten of eleven CI
  workflows are `workflow_dispatch`-only including `ci.yml` itself, so a green
  history is not evidence that a merged commit was checked"*). Filed anyway per
  severity rule 4; the report should mark it conceded rather than new. The
  disagreement is only about `CLAUDE.md`, which still asserts the opposite and is
  the document a contributor reads.
- I did not check whether a local pre-commit hook exists and is installed.
  `CLAUDE.md` refers to one for `cargo fmt`; a hook is per-clone and opt-in, so it
  is not a gate in the sense this finding is about, but it would change the
  practical risk.
- I did not run `cargo deny`, `cargo audit`, `clippy` or `reuse lint` in this
  engagement, so I make no claim about whether the tree would pass them today.
