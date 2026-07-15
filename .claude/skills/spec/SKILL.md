---
name: spec
description: Codespeak-style module specs for Lakko. Generates and maintains plain-language behavioral specs (`<module>.sc.md`, co-located with the source) so a human reviews the readable spec-diff alongside the code-diff. Use to author a spec for a covered module, update it when the module changes (mandatory — the spec-drift gate enforces it), or check for drift.
---

# Lakko codespeak module specs

A `.sc.md` spec is a plain-language, behavioral description of what a module GUARANTEES — written so a reviewer can review the spec-diff (readable) alongside the code-diff (hard). It is generated FROM the code and kept in lock-step with it. A drifted spec is worse than none — it lies — so the drift gate (`just spec-check`) fails CI when a covered module's source changes without its spec changing.

## Naming + location

- One spec per module, co-located: `crates/lakko-calls/src/header.rs` → `crates/lakko-calls/src/header.sc.md`.
- The `.sc.md` suffix marks it as a generated codespeak spec (not hand-authored prose docs, not an ADR).
- The stem must match the source file stem exactly — the drift gate pairs them by stem.

## Granularity — what is NECESSARY vs merely sufficient

- **Cover: modules that carry a behavioral unit** — a state machine, a wire parser, a crypto pipeline, a public contract with invariants. This is where reviewing a diff is hardest and the stakes highest.
- **Do NOT cover: trivial files** — `error.rs` (an enum), `lib.rs` (re-exports), `mod.rs`, generated code, or a module that is a thin pass-through. A spec there is noise that still incurs the drift tax.
- **Do NOT cover: tests** — they already are executable specs.
- **Granularity is per-module (per source file), never per-line and never per-crate-only.** A file is the behavioral unit; a crate is too coarse (the CLAUDE.md module map already covers crate grain), a line is churn.

### Coverage tiers (extend in this order)

1. **Tier 1 (necessary):** security-critical Rust — `lakko-mls`, `lakko-crypto`, `lakko-calls`, `lakko-transport`, `lakko-contacts`, `lakko-channels`, `lakko-transparency`, `lakko-attest`, the wire/parser modules, and the FFI crates.
2. **Tier 2:** `services/`.
3. **Tier 3 (low value):** clients (they get UX specs; the code is more mechanical).

## Format — behavioral, NOT a signature restatement

The value is the WHY and the guarantees, not a re-typing of function signatures. Use this template (see `crates/lakko-calls/src/header.sc.md` for the reference pilot):

```
<!-- SPDX + a GENERATED SPEC marker line -->
# `<module>` — <one-line title> (spec)
**Module:** <path>   **One line:** <what it does>

## Why this module exists         — the problem it solves, load-bearing role
## Public contract                — each entry point + what it GUARANTEES, in prose
## <Wire layout / States>         — if it parses bytes or is a state machine
## Invariants upheld              — link to I/C/T ids; what always holds
## Failure modes                  — every error and when; panic/no-panic posture
## What this module does NOT do   — the boundary; fail-closed guarantees; "cannot happen"
## Attacker exposure              — untrusted inputs, the bound, the fuzz target
## References                     — ADRs, invariant ids, key tests
```

Omit sections that don't apply (a pure state machine has no "Wire layout"). Keep it tight — prose + lists, no pixel-level detail, no line-by-line echo of the code.

## Honesty rules (non-negotiable, from CLAUDE.md)

- **Read the module before writing the spec.** Every claim must match what the code actually does. Never fabricate a guarantee, an invariant, or a test name.
- **Verify referenced ids.** If you cite C-5 or ADR-0013, it must actually be the relevant one — don't guess.
- **English only. No emoji.** SPDX header on every `.sc.md`.
- If the code does something the spec would be embarrassed to state (an unchecked unwrap on attacker input, a silent downgrade), that is a FINDING — surface it, don't launder it into confident prose.

## The drift gate (how specs stay honest)

- **Flow step (soft):** whenever you change a covered module, update its `.sc.md` in the SAME change. This is a mandatory step, like updating a test.
- **CI gate (hard):** `just spec-check` (→ `tools/spec-drift-check.sh`) fails when a source file changes and a sibling `<stem>.sc.md` exists but is NOT in the same changeset. It only checks modules that already have a spec, so coverage grows safely without a big-bang migration. Run it locally before handing off; it runs in CI on every PR.
- To COVER a new module: add its `.sc.md`. To retire coverage: delete the `.sc.md` (and say why).

## Place in the flow

- **After a covered module's code lands (or in the same step):** regenerate/patch its `.sc.md` so the behavioral description matches. In the `lakko-dev` coordinator flow this rides alongside the Rust-core and verify steps.
- **Reviewer-facing:** the point is that a human reads the `.sc.md` diff first to understand the change in plain language, then spot-checks the code against it. Keep the spec worth reading.
