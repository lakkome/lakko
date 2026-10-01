<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — security documentation

The audit-readiness pack. Its purpose is to let a reviewer load the project state in
one read pass without grepping the codebase; every file points at the live code paths
it summarises, so a claim here can be checked by jumping to the citation.

**If you are here to scope or start an external cryptographic review, read
[`external-review-scope.md`](./external-review-scope.md) instead of this file.** It
carries the boundaries, the sizes, a reading order, what has already been verified,
and what a reviewer must not assume. This file is the map of the pack.

**Currency.** Reconciled against the working tree on **2026-09-02**. The previous
revision was written 2026-05-27 and had drifted into stating things that were no
longer true — it described the TUF verifier as refusing delegated targets after the
delegation walk had landed, three transports as unimplemented scaffolds after all
three shipped, and byte-identical reproducible builds that are in fact an advisory
job with its schedule disabled. Treat an old revision of this file as a lead, not a
fact, and see §*Provenance* in the scope brief.

## Files in this pack

Read the first three in order; they are 490 lines and are enough to orient.

- [`threat-model.md`](./threat-model.md) — assets, adversaries, the seven security
  invariants. The single source of truth for "what must the protocol resist".
- [`crypto-overview.md`](./crypto-overview.md) — every primitive in use, where it
  lives in the code, which RFC or cipher-suite it implements.
- [`mls-state-machine.md`](./mls-state-machine.md) — the OpenMLS wrappers Lakko
  ships, the exporter labels it coined, and the invariants the state machine must
  preserve (**I-1..I-8**).

Then, as references rather than narratives:

- [`invariant-index.md`](./invariant-index.md) — the consolidated cross-reference for
  every numbered invariant and every tracked gap. **Open it when you meet an ID you
  do not recognise; do not read it front to back.** It also documents the procedure
  for adding an invariant without drifting from the source files.
- [`calls.md`](./calls.md) — the `C-` class. Despite the filename this is the
  **catch-all**, not only the call pipeline: it holds the media-key chain, the
  call-admission JWT, link-based invites, the SFU planes, channel broadcast, key
  transparency, backup rollback, the iOS background-decrypt boundary, push routing,
  and the ADR-0045 group-membership rules. Reach it through the index.
- [`transport.md`](./transport.md) — the `T-` class. Anti-censorship and rendezvous
  transport: pluggable-kind identifiers, the four-stage bootstrap verify, the DoH
  trust chain, the AEAD stream primitive, Shadowsocks-2022, ECH, REALITY,
  WebTransport, MASQUE, and the no-unprotected-fallback rule.
- [`auth-flow.md`](./auth-flow.md) — OPAQUE-PAKE registration / login / recover,
  WebAuthn passkey opt-in, the device-key derivation chain, key-rotation paths.
- [`device-keys.md`](./device-keys.md) — the hardware-backed device-root-key
  lifecycle per platform (Secure Enclave / StrongBox / non-extractable WebCrypto).
- [`backup.md`](./backup.md) — the encrypted backup envelope, the Argon2id KEK, the
  recovery flow, Shamir social recovery. Note two independent version axes: the
  **envelope** is v1 and the **payload** inside it is v3. The v2 envelope's
  tamper-evident chain is **dormant** — wired to no FFI and no client — and rollback
  is instead closed by the auth-signed backup head of
  [ADR-0037](../adr/0037-backup-head-rollback-commitment.md).
- [`multi-device.md`](./multi-device.md) — two-tier identity
  ([ADR-0007](../adr/0007-multi-device-identity.md)), DeviceGroup pairing,
  remove-device, the history catch-up bundle, cross-device sync.
- [`known-gaps.md`](./known-gaps.md) — the `G-` class: what is not audit-ready, so a
  reviewer does not spend the engagement rediscovering it. Each entry states what it
  is, why it is open, and what closes it.

## The numbers

Produced by `just audit-index-check`, which fails CI if an invariant or gap is
pinned in its source document but missing from the index, or the reverse:

| Class | Count | Range | Source |
|---|---|---|---|
| **I-** | 8 | I-1..I-8 | [`mls-state-machine.md`](./mls-state-machine.md) |
| **C-** | 70 | C-1..C-70 | [`calls.md`](./calls.md) |
| **T-** | 16 | T-1..T-16 | [`transport.md`](./transport.md) |
| **G-** | 39 | G-1..G-39 | [`known-gaps.md`](./known-gaps.md) |

The gate cross-checks the summary *numbers* only for the `G-` class. A stale `T-`
count therefore survived in the index from the T-16 commit until 2026-09-02, which is
worth knowing before trusting a count that no tool produced.

## The one open item that blocks release

**[G-39](./known-gaps.md) — the external cryptographic review has not happened.** It
is security invariant #3's third clause, and it is a **release gate**: no build
reaches users, no store submission, no GA tag, while it is open. It is owned by
[ADR-0046](../adr/0046-external-crypto-review.md), which moved it out of four
scattered ADR sentences into one tracked place, and scoped for an engagement in
[`external-review-scope.md`](./external-review-scope.md).

The per-change half of invariant #3 — an ADR plus property tests before an
MLS-touching change lands — is unchanged and is still enforced per change.

## Reproducibility

```bash
just ci     # fmt-check + lint + test + cargo deny + SPDX + fuzz smoke + index gate + spec gate
just test   # cargo test --workspace
just lint   # clippy --workspace -- -D warnings
just audit  # cargo deny check
just reuse  # SPDX-header lint
just fuzz-check   # smoke-build every fuzz harness on stable
```

`rust-toolchain.toml` pins an exact stable release rather than a floating channel;
[`infra/nix/flake.nix`](../../infra/nix/flake.nix) pins the whole toolchain.

Two things a reviewer should know before reading a green CI badge as assurance:

- **Ten of eleven GitHub workflows are `workflow_dispatch`-only**, including
  `.github/workflows/ci.yml` — its `push` and `pull_request` triggers are commented
  out, a May 2026 quota-conservation decision. The only automatically-triggered
  workflow is `spec-drift.yml`. The gates above run when someone runs `just ci`
  locally or dispatches the workflow by hand.
- **Byte-identical reproducible builds are advisory.**
  `.github/workflows/repro.yml` carries `continue-on-error: true` on the Rust and
  cross-runner jobs (the web job gates), and its weekly cron is disabled.

## Cross-links

- Architectural decisions: [`docs/adr/`](../adr/)
- Tactical decomposition: [`docs/ROADMAP.md`](../ROADMAP.md)
- Phase-3 closeout punch list: [`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md)
