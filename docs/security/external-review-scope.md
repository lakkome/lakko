<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Lakko — external cryptographic review: scope brief

**Status:** the engagement has not started. This document exists so it can be
scoped and quoted without a discovery phase.

**Tracked as [G-39](./known-gaps.md), owned by
[ADR-0046](../adr/0046-external-crypto-review.md).** G-39 states the obligation and
the release gate; this document states the *engagement* — boundaries, sizes,
reading order, what has already been done, and the questions we most want answered.

**The gate, so a reader does not have to infer its force: no build reaches users
while G-39 is open.** No store submission, no GA tag. Lakko has no production
deployment and no users today, so nothing is currently at risk from the gap being
open — but nothing ships until it closes either.

---

## 1. What Lakko is, in one paragraph

A Telegram-class messenger with end-to-end encryption for everything — 1×1, groups,
channels, audio and video calls — against a **zero-trust server**. MLS (RFC 9420) is
the single group-keying mechanism for all of it, including call media keys, via an
exporter. Clients are native (iOS, Android, web) over one shared Rust core. The
server relays ciphertext and public artefacts and is never trusted with key
material. Seven security invariants govern the design; they are stated in
[`threat-model.md`](./threat-model.md) and every numbered invariant in
[`invariant-index.md`](./invariant-index.md) cites which of them it serves.

---

## 2. What is being reviewed

### 2.1 Primary — the MLS group-membership state machine

This is the reason the engagement exists, and it is a **surface, not a change**. The
interactions between these pieces are the point; a per-change review sees each as a
slice.

| Piece | Where |
|---|---|
| The per-proposal authorisation rule, written once and consumed at three layers (receipt gate, sweep filter, peer-verified commit gate) | [`crates/lakko-mls/src/group_proposals.rs`](../../crates/lakko-mls/src/group_proposals.rs) |
| `owner_survives` — the one rule that judges a **commit** rather than a proposal (**C-63**) | same file |
| The staged-sweep lifecycle: stage / merge / abandon, and its per-group rollback | [`crates/lakko-mls/src/group.rs`](../../crates/lakko-mls/src/group.rs) |
| The `create_application_message` pending-set park (a stored proposal would otherwise mute the whole group's chat) | same file |
| The committer election over the role table (`committer_rank`) | [`crates/lakko-mls/src/group_roles.rs`](../../crates/lakko-mls/src/group_roles.rs) |
| Approval-as-proposal (ADR-0045 S7) — the last direct-commit path removed | [`crates/lakko-mls/src/group.rs`](../../crates/lakko-mls/src/group.rs) |
| Typed refusal reasons and their three-valued disposition | [`crates/lakko-mls/src/refusal.rs`](../../crates/lakko-mls/src/refusal.rs) |
| The `0xFF02` GroupContext role-table extension everything above layers on | [`crates/lakko-mls/src/group_roles.rs`](../../crates/lakko-mls/src/group_roles.rs) |

Governing documents: [ADR-0022](../adr/0022-group-admin-roles.md) (role table),
[ADR-0027](../adr/0027-external-join-approval.md) (external join),
[ADR-0040](../adr/0040-durable-pending-commit-outbox.md) (crash-safe commits),
[ADR-0045](../adr/0045-remote-commit-epoch-fork.md) (the arc itself).
Invariants: **I-1..I-8** and **C-63..C-69**.

### 2.2 Primary — three independent client implementations of the same disciplines

**Not incidental, and the reason C-69 exists:** a discipline can be correct in the
core and attached to the wrong end in a client, silently. Each of these rules is
implemented three times, once per platform, from one specification:

`AuthorIdentity` · `CommitterBeacon` · `CommitterSweep` · `DurableCursor` ·
`MembershipEnded` · `MembershipRefusal` · `MembershipRetry` · `OwnershipTransfer` ·
`PendingRequests` · `UnappliedRequests` · `RemoveMemberConfirm`

- web — [`apps/web/src/chat/`](../../apps/web/src/chat/) (`.ts`)
- iOS — [`apps/ios/Lakko/Chat/`](../../apps/ios/Lakko/Chat/) (`.swift`)
- Android — [`apps/android/app/src/main/kotlin/me/lakko/app/chat/`](../../apps/android/app/src/main/kotlin/me/lakko/app/chat/) (`.kt`)

Divergence between the three is a defect class of its own, and three of the seven
findings in the most recent internal review were exactly that.

### 2.3 Secondary — the cryptographic primitives the above rests on

| Area | Crate | Invariants |
|---|---|---|
| Primitives (HKDF, AEAD, Argon2id, BLAKE3, X25519, Ed25519) | `lakko-crypto` | — |
| Call media keys — MLS exporter → CallKey → per-`(leaf, stream)` key → SFrame | `lakko-calls` | C-1..C-14, C-37, C-38, C-45..C-50, C-53, C-55 |
| Attestations — four fixed-layout Ed25519 kinds under one signing key | `lakko-attest` | C-56, C-58 |
| Key transparency — Merkle log, consistency proofs, witness co-signing | `lakko-transparency` | C-56, C-57 |
| Envelopes reachable from an untrusted relay | `lakko-contacts` | C-24..C-36, C-60..C-62 |
| Encrypted backup | `lakko-backup` | C-58 |

### 2.4 Explicitly NOT in this engagement

- **Deployment and operations.** Nothing here is deployed to users; see §7 for what
  that means for a reviewer's assumptions.
- **The anti-censorship transport stack** (REALITY, MASQUE, Shadowsocks-2022,
  WebTransport, ECH). Fully implemented and tested in-process, invariants
  **T-1..T-16** — but on no wire anywhere, and orthogonal to the MLS surface. It
  deserves its own engagement with a censorship-lab component.
- **`lakko-tuf`** — a complete TUF v1.1 verifier with **no production consumer**.
  The shipping update mechanism is [ADR-0038](../adr/0038-update-available-notice.md)'s
  signed notice, which is itself dormant behind default-empty key pins.
- **Web-application-level concerns** (XSS, CSP, supply chain of the npm tree) unless
  they bear on key handling.

---

## 3. Size, so this can be quoted

Measured 2026-09-02. Rust line counts separate production code from inline
`#[cfg(test)]` modules, because Lakko keeps most tests beside the code.

| Crate | Code | Inline tests |
|---|---|---|
| `lakko-mls` | 7 206 | 6 373 |
| `lakko-contacts` | 5 076 | 4 294 |
| `lakko-calls` | 4 319 | 4 361 |
| `lakko-transparency` | 1 556 | 1 221 |
| `lakko-backup` | 1 413 | 688 |
| `lakko-attest` | 1 346 | 865 |
| `lakko-crypto` | 757 | 491 |
| **Total** | **21 673** | **18 293** |

`lakko-mls` alone, by file: `group.rs` 6 437 · `device_group.rs` 1 666 ·
`publisher_group.rs` 968 · `group_proposals.rs` 941 · `history_placeholder.rs` 614 ·
`identity.rs` 612 · `group_roles.rs` 499 · `refusal.rs` 426 · `history_bundle.rs` 327 ·
`key_package.rs` 296 · `pairing.rs` 265 · `welcome_rendezvous.rs` 206 · `lib.rs` 148 ·
`error.rs` 120 · `ciphersuite.rs` 54.

For orientation on the rest of the repository, which is **not** in scope but sets the
context: `crates/` 121 990 · `services/` 43 965 · web 85 787 · iOS 104 166 ·
Android 114 410.

Documentation the reviewer may draw on: 490 lines to orientation (§4), 10 669 lines
of security documents, 18 016 lines of ADRs.

---

## 4. Reading order

The security documents total over ten thousand lines and two of them are over four
thousand each. Read in this order; the first three are 490 lines and are enough to
start.

1. [`threat-model.md`](./threat-model.md) — assets, adversaries, the seven
   invariants. What must resist what.
2. [`crypto-overview.md`](./crypto-overview.md) — every primitive, where it lives,
   which RFC.
3. [`mls-state-machine.md`](./mls-state-machine.md) — the OpenMLS wrappers, the
   exporter labels we coined, **I-1..I-8**.
4. [`invariant-index.md`](./invariant-index.md) — the cheat sheet. Every numbered
   invariant with a pointer to the file and section that describes it. Open this
   when you meet an ID you do not recognise; do not read it front to back.
5. [ADR-0045](../adr/0045-remote-commit-epoch-fork.md), then
   [ADR-0022](../adr/0022-group-admin-roles.md) — the primary surface, and the
   layer beneath it.
6. **C-63..C-69** in [`invariant-index.md`](./invariant-index.md) — the rules the
   arc minted, each with what it does *not* claim.
7. [`known-gaps.md`](./known-gaps.md) — **G-39** first (it is this engagement), then
   whatever a `C-` or `T-` entry sent you to. Not front to back.

`calls.md` is a reference, not a narrative: the `C-` class became the catch-all for
every non-MLS, non-transport invariant, so it holds the call pipeline *and* key
transparency *and* backup *and* push routing *and* the ADR-0045 membership rules.
Reach it through the index.

---

## 5. What has already been done

Stated so the engagement is not paid to repeat it, and so the reviewer knows where
our own confidence is thin.

### Tests — measured 2026-09-02, not estimated

`cargo test --workspace`: **2 369 passed, 0 failed, 14 ignored.**

| Crate | Tests |
|---|---|
| `lakko-mls` | 268 (lib 172 · proposals 37 · refusal reasons 21 · ownership transfer 12 · admin roles 8 · group-id width 8 · end-to-end 5 · membership state 5) |
| `lakko-transport` | 251 (8 ignored: 1 live-network, 7 censorship-lab-gated) |
| `lakko-calls` | 178 |
| `lakko-svc-sfu` | 89 (2 ignored: dockerised live-SFU) |
| `lakko-transparency` | 71 |
| `lakko-tuf` | 62 |
| `lakko-backup` | 44 |
| `lakko-attest` | 39 |

All 14 ignored tests, enumerated rather than characterised, because the one
interesting case is invisible under a summary: **7** censorship-lab observer tests and
**3** dockerised end-to-end tests (two live-SFU, one KT-log recovery pipeline) need
infrastructure a checkout does not have; **1** resolves a real domain over DoH; **2**
are `ignore`d documentation examples in `lakko-contacts`, not behaviour tests. The
fourteenth is different and worth a reviewer's attention:
`masque_tunnel_relays_http_request_to_internal_origin` is ignored for a **semantic**
reason, not a missing dependency — MASQUE's CONNECT-UDP carries unreliable, unordered
datagrams, so relaying a reliable HTTP service over it does not work, and the response
is dropped on close. The `MasqueListener` is deliberately not wired into
`services/obfs-proxy` for that reason. That is a design conclusion recorded as a
skipped test, which is a reasonable place to record it and an easy one to misread as
an infrastructure gate.

### Fuzzing — 64 targets across 18 crates and one service

Every attacker-touchable parser has one. Densest where the attack surface is:
`lakko-contacts` 12 · `lakko-transparency` 8 (one per ADR-0019/0020/0039 verifier) ·
`lakko-mls` 8 · `lakko-tuf` 5 · `lakko-transport` 5 · `lakko-channels` 4 ·
`lakko-attest` 4 · `lakko-proto` 3 · `lakko-call-jwt` 3 · `lakko-backup` 3 ·
`lakko-calls` 2 · one each for `services/sfu`, `lakko-sync`,
`lakko-push`, `lakko-mnemonic`, `lakko-auth-token`, `lakko-attachments`.

**These are smoke-built in `just ci` but only *run* on a manual dispatch** of
`.github/workflows/fuzz-quick.yml` (60 s per target by default). No corpus is
committed. Treat the coverage as "a harness exists and does not immediately crash",
not as "this has been fuzzed at length".

### Cross-stack agreement, pinned by a shared golden vector

One SFrame wire vector is pinned in **seven** places, so the six independent SFrame
implementations cannot drift apart silently:
[`crates/lakko-calls/src/sframe.rs`](../../crates/lakko-calls/src/sframe.rs) (core) ·
[`crates/lakko-sframe-native/tests/golden_cross_stack.rs`](../../crates/lakko-sframe-native/tests/golden_cross_stack.rs) (C ABI) ·
[`apps/shared/sframe-bridge/test_bridge.cc`](../../apps/shared/sframe-bridge/test_bridge.cc) (C++ bridge) ·
[`apps/ios/LakkoTests/SFrameGoldenTests.swift`](../../apps/ios/LakkoTests/SFrameGoldenTests.swift) ·
[`apps/android/app/src/androidTest/kotlin/me/lakko/app/calls/SframeGoldenTest.kt`](../../apps/android/app/src/androidTest/kotlin/me/lakko/app/calls/SframeGoldenTest.kt) ·
[`apps/web/src/calls/sframe-golden.test.ts`](../../apps/web/src/calls/sframe-golden.test.ts) (wasm) ·
[`services/sfu/tests/synthetic_client_e2e.rs`](../../services/sfu/tests/synthetic_client_e2e.rs).

This is the headless half of **G-26**. The remaining half — a real RTP round-trip
between two platforms through a deployed SFU — has never been run.

### An internal security review, and what it found

A `lakko-security` pass over the ADR-0045 arc (2026-08-29) returned **three HIGH
findings plus a confirmed impersonation**, seven in total, all since closed. They are
recorded in ADR-0045's *What the security review found* section and indexed as
**C-66**, **C-67** and **C-68**.

**We offer this as the argument for an external review rather than against one:
none of the seven needed an attacker, and four needed nobody at all.** A network
blip, a stale roster, two admins doing an ordinary thing, or simply enough time
passing. Specifically: a KeyPackage expiring while a join request waited would freeze
a group's membership permanently and invisibly, while chat kept working over it. An
external reviewer does not share the blind spots that let those ship.

---

## 6. The questions we most want answered

Ranked. A reviewer is free to ignore this ranking; it records where we think the risk
is, which is itself information.

1. **Do the three layers of the authorisation rule compose?** By-reference proposals
   are judged against the proposer, by-value against the committer. We have tests for
   each layer and for the split. We have not systematically tested **compositions** —
   a commit carrying both kinds, a proposal authorised at receipt whose author is
   demoted before the sweep, a sweep that drops one proposal of a pair.
2. **Is C-66's enumeration sound?** It lists fifteen conditions under which
   `CommitBuilder::build` fails hard, because an unbuildable sweep is a permanent,
   invisible, group-wide membership freeze. That enumeration is a **mirror of openmls
   rules Lakko does not own**. It is backed by an exclusion search that needs to know
   only *which* proposal is uncommittable and never *why* — we believe that makes the
   enumeration a performance optimisation rather than a correctness dependency, and
   we would like that belief tested.
3. **Do the cross-invariant interactions hold?** Each of C-63..C-69 was reasoned
   about locally. C-63 (owner survives), C-65 (one role change in flight), C-66 (the
   owner's reserve past the pending cap) and ADR-0022's role gate all constrain the
   same commit, and no test exercises them together.
4. **Is the exporter-derived call key chain sound end to end?** MLS exporter →
   `CallKey` → per-`(leaf, stream)` key → SFrame seal, plus the overlap keyring on a
   mid-call epoch advance. Two prior defects in this area were found internally: a
   cross-stream nonce reuse (ADR-0034) and then its incomplete fix (ADR-0035).
5. **Do the three client implementations agree?** See §2.2. We would rather be told
   they diverge than told the core is correct.
6. **Is the failure direction right everywhere?** Invariant #4 says no fallback to
   non-E2EE. Several places deliberately fail *open* instead — an absent backup head,
   the call-admission anti-spam throttle, the update notice. Each has a stated
   argument; we would like them checked as a set rather than one at a time.

---

## 7. What a reviewer must not assume

Stated plainly, because each of these would otherwise be a reasonable inference.

- **Nothing is deployed.** There are no users and no production data. Only
  `auth`, `ds`, `storage`, `discovery`, `push-gw`, `channels` and `sfu` have a
  deployment path at all
  ([`infra/scripts/deploy.sh`](../../infra/scripts/deploy.sh)). **`transparency`
  and `witness` have no systemd unit and are in no deploy target, and `obfs-proxy`
  runs only on the test stack (a unit since 2026-10-30)** —
  the key-transparency log that invariant #6 rests on runs only in a docker compose
  stack for end-to-end tests.
- **CI does not gate pull requests.** Of eleven workflows, **ten are
  `workflow_dispatch`-only**, `.github/workflows/ci.yml` among them — its `push` and
  `pull_request` triggers are commented out (a May 2026 quota-conservation
  decision). The only automatically-triggered workflow is `spec-drift.yml`. Lint,
  tests, `cargo deny` and the fuzz smoke-build run when someone runs `just ci`
  locally or dispatches the workflow by hand. **A green history is not evidence that
  every merged commit was checked.**
- **Reproducible builds are advisory, not proven.** `.github/workflows/repro.yml`
  carries `continue-on-error: true` on both the Rust and cross-runner jobs (the web
  job does gate), and its weekly cron is disabled. The blocker it was waiting on — a
  non-reproducible uniffi rlib — was resolved by the uniffi 0.31 bump; what remains
  is one green dispatch.
- **Several flags default to off**, so a shipped client is not exercising the
  strongest posture: `LAKKO_TRANSPARENCY_REQUIRE_GENESIS_ATTEST` (off),
  the witness-quorum threshold (defaults to 1, i.e. single-witness),
  and every witness / release-manifest client pin (empty by default, which disables
  the gate rather than failing it). `LAKKO_TRANSPARENCY_REQUIRE_LEAF_AUTH` does
  default on.
- **A module doc may describe a state the code has left.** Six such comments were
  found while assembling this brief and swept on 2026-09-02 — `lakko-tuf` claimed to
  refuse delegated targets after the walk had landed; three transport modules claimed
  `connect` returns `NotImplemented`; `services/sfu` claimed RTP fanout and the media
  loop had not landed; `services/obfs-proxy` claimed MASQUE and WebTransport have no
  listener while wiring one of them a few lines further down. **That the sweep
  happened is not a guarantee that the class is closed** — these were found by
  chasing one document's claims, not by an exhaustive pass, and the pattern is
  predictable: a "What is NOT here yet" section written during a scaffold sprint and
  never revisited when the sprint that filled it landed. **Trust the code over its
  comment, and tell us when you find one we missed.**

---

## 8. Building and reproducing

```bash
just ci     # fmt-check + lint + test + cargo deny + SPDX + fuzz smoke + index gate + spec gate
just test   # cargo test --workspace
just lint   # clippy --workspace -- -D warnings
just audit  # cargo deny check
```

The toolchain is pinned to an exact stable release in `rust-toolchain.toml`, not a
floating channel. [`infra/nix/flake.nix`](../../infra/nix/flake.nix) pins the whole
toolchain for a hermetic build.

Two gates worth knowing about, because they are unusual and they are what keeps this
documentation honest:

- `just audit-index-check` cross-checks that every numbered invariant and gap is
  pinned in **both** its source document and the index. It is what produces the
  authoritative counts quoted throughout: **8 I + 69 C + 16 T invariants + 39 G
  gaps**. (It cross-checks the summary *numbers* only for the `G-` class, which is
  how a stale `T-` count survived in the index until 2026-09-02.)
- `just spec-check` fails if a module carrying a codespeak specification changes
  without its specification changing. It is the one gate that runs on every pull
  request.

Fuzzing:

```bash
just fuzz-list                 # every target
just fuzz <crate>/fuzz <name>  # run one (nightly + cargo-fuzz)
just fuzz-quick                # every target, 60s each
```

---

## 9. Provenance of this document

Every count, path and line reference here was verified against the working tree on
2026-09-02 by reading the code or running the command, not by reading another
document. Where a fact could not be established it is absent rather than estimated.

The one thing this document cannot verify about itself is that it stays true. The
audit-readiness pack's previous entry point was written on 2026-05-27 and by
2026-09-02 carried at least a dozen false claims — including telling a reviewer to
verify that the TUF verifier refuses delegated targets, a property removed when the
delegation walk landed. If this file is more than a few months old when you read it,
check its claims before relying on them, and tell us which have rotted.
