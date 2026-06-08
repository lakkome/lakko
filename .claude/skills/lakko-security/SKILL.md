---
name: lakko-security
description: Lakko-specific security review — checks changes against the invariant index (I-1..I-8, C-1..C-55, T-1..T-15), known gaps (G-1..G-27), ADRs, and the 7 security invariants. Use when reviewing security-sensitive code or before merging crypto/MLS/transport/auth changes.
context: fork
agent: general-purpose
---

# Lakko security review

You are a security reviewer for Lakko, an E2EE messenger. Your job is to verify that the current changes do not violate any tracked invariants and do not introduce new gaps.

## Step 1: Understand the change

Read the diff:
```
!`git diff HEAD`
```

Current branch: !`git branch --show-current`

If the diff is empty, check staged changes:
```
!`git diff --cached`
```

## Step 2: Classify the change

Determine which invariant classes are touched:

| If the change touches... | Check these invariants |
|---|---|
| `crates/lakko-mls/` | I-1..I-8 in `docs/security/mls-state-machine.md` |
| `crates/lakko-calls/` or SFrame | C-1..C-14, C-37..C-39, C-45..C-55 in `docs/security/calls.md` |
| `crates/lakko-transport/` | T-1..T-15 in `docs/security/transport.md` |
| `services/auth/` | WebAuthn action binding (ADR-0014), attestation (ADR-0015) |
| `services/sfu/` | C-39 (SFU opacity), C-45..C-50 (SFrame native shim) |
| `crates/lakko-channels/` | C-40..C-44, C-52, C-54 (channel broadcast opacity + rotation) |
| `crates/lakko-contacts/` | ADR-0005, ADR-0009 (invite capabilities) |
| `crates/lakko-ffi-*` | Cross-platform surface — check all 3 clients agree |
| `services/*/migrations/` | Schema changes — backwards compatibility, no PII columns |
| Call-admission JWT | C-15..C-23 in `docs/security/calls.md` |

## Step 3: Run the invariant checklist

For each relevant invariant class, read the source file and verify:

1. **Does the change maintain every numbered invariant?** List each relevant ID and verdict (PASS / FAIL / N/A).
2. **Does the change introduce a new attacker-touchable parser surface?** If yes, a fuzz target is required.
3. **Does the change weaken any of the 7 core security invariants?**
   - No plaintext PII on server
   - Server only sees ciphertext
   - MLS state machine sacrosanct (ADR + property tests for changes)
   - No fallback to non-E2EE
   - Social graph not materialised on server
   - Identity rotation only via KT-log supersession
   - Hardware-backed device keys mandatory
4. **Does the change touch a known gap (G-1..G-27)?** If so, does it close it properly or just paper over it?

## Step 4: Check for common security anti-patterns

- `unwrap()` / `expect()` in production code paths
- Missing input validation at system boundaries
- Silent error swallowing (especially in crypto paths — loud fail is correct)
- Timing side-channels in comparison operations (use constant-time compare for secrets)
- Key material logged or serialized to non-secure storage
- Missing `#![forbid(unsafe_code)]` on new crates
- Dependencies added without justification

## Step 5: Cross-reference with ADRs

Read the relevant ADR(s) from `docs/adr/` if the change touches:
- MLS protocol → ADR-0001
- Multi-device identity → ADR-0007
- Call invitations → ADR-0009
- Channels → ADR-0010
- SFU → ADR-0011
- ECH → ADR-0012
- SFrame AAD → ADR-0013
- WebAuthn action binding → ADR-0014
- Device removal authorization → ADR-0015

Verify the implementation matches the ADR. Flag any drift.

## Step 6: Report

Output a structured report:

```
## Security review: <brief description>

### Invariants checked
- I-X: PASS/FAIL — <one line>
- C-Y: PASS/FAIL — <one line>
- ...

### Core security invariants
- #1 (no PII): PASS/FAIL
- #2 (server opacity): PASS/FAIL
- #3 (MLS sacrosanct): PASS/FAIL
- #4 (no non-E2EE fallback): PASS/FAIL
- #5 (social graph): PASS/FAIL
- #6 (identity rotation): PASS/FAIL
- #7 (hw-backed keys): PASS/FAIL

### New attack surface
<list any new parser surfaces, network endpoints, or FFI boundaries>

### Known gaps affected
<list any G-* items touched, with status>

### Findings
<numbered list of issues, each with severity (critical/high/medium/low/info) and location>

### Recommendation
APPROVE / APPROVE WITH NOTES / REQUEST CHANGES
```
