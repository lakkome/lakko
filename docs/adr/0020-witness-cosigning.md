<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0020: Witness co-signing for KT-log split-view resistance (G-30 #1)

- **Status:** Accepted — Sprints 1-6 landed (Merkle consistency proof + witness
  co-sig format + FFI + `services/witness` + transparency integration + the
  3-client co-sig gate + the live recovery-e2e). Only the `lakko-security` gate
  on the audited-core changes remains before merge. 2026-06-13
- **Date:** 2026-06-13
- **Context:** The KT log (`services/transparency` + `crates/lakko-transparency`
  + the 3-client verify) is operator-signed + append-only, and the G-30 #2 chain
  verifier (ADR-0019) makes a *leaked-old-key* equivocation **detectable**. But a
  malicious **operator** — it holds the root signing key — can still sign TWO
  different valid roots and show each to a different victim (a **split-view** /
  fork). Each client sees a self-consistent, monotone view (its own no-rollback
  `log_size` high-water passes) and CANNOT detect the fork: today only the
  operator signs a root, and each client only compares a fetched root to its OWN
  last-seen `log_size`. This is G-30 finding 1 / ADR-0016 §equivocation, listed
  as "not built".

## Threat

A compromised / dishonest operator forks the log: it builds two append-only-
valid trees that diverge (the same `log_size` with different `tree_root`, or two
incompatible histories), signs both with its single key, and serves tree A to
the victim + tree B to everyone else. Every client's local check passes (each
view is internally monotone + operator-signed), so the victim is pinned to a
forged identity that no honest observer can see. The chain verifier (ADR-0019)
does not help — it anchors a rotation to *a* signed root, and both forks carry
valid operator-signed roots.

## Decision

An independent **witness** co-signs every operator root, but ONLY after
verifying a Merkle **consistency proof** (RFC 6962 §2.1.2) that the new root
append-only-extends the witness's last-co-signed root. Clients require BOTH the
operator signature AND a valid co-signature from a build-time-pinned witness
before trusting a root (fail-closed, invariant #4).

To fork, the operator would need the witness to co-sign two
mutually-inconsistent roots. The honest witness refuses: it persists only its
single last-co-signed `(root, log_size)` and co-signs a new root only when the
operator's consistency proof shows the new root extends THAT root — and two
divergent forks cannot both extend one root. So a split-view now requires
operator **and** witness collusion, in two independent trust domains.

This is the Sigsum / CT-witness model: the witness verifies consistency, it does
NOT mirror the full log (it stores one root + verifies one operator-supplied
proof per co-sign — light).

### Load-bearing decisions

- **Single witness first; K-of-N a config slot for later.** The residual is
  operator+witness collusion — documented; a witness threshold + the deferred
  client-gossip layer (the other carrier from the G-30 #1 design) are the later
  hardenings.
- **Independence is an OPS property, not code-enforced.** The codebase ships the
  witness as a separate `services/witness`; running it in a DIFFERENT trust
  domain (host + operator) is the deployment requirement for the guarantee. The
  e2e may co-locate for testing; production independence is a deploy decision.
- **Fail-closed, with a transition fallback.** A root lacking a valid
  pinned-witness co-sig is untrusted → the client refuses. When no witness key
  is pinned (the integration unconfigured), clients keep today's operator-only
  posture so the rollout is staged.

## Components (sprint phasing)

1. **Merkle consistency proof** (`crates/lakko-transparency/src/merkle.rs`) — the
   missing RFC 6962 §2.1.2 primitive (`Error::ConsistencyProofMismatch` already
   exists, unused). `consistency_proof` + `verify_consistency`, beside the
   existing inclusion code, on the same RFC 6962 tree. Property tests (every
   `0 < m < n` round-trips; a same-size fork / wrong root fails) + a fuzz target.
2. **Witness co-signature** (`crates/lakko-transparency`) — a compact
   `lakko-attest`-style blob `version(1) ‖ tree_root(32) ‖ log_size(8 BE) ‖
   witness_sig(64)`, signed over a DISTINCT domain
   (`lakko/transparency/witness/v1`) so an operator sig and a witness co-sig
   never cross-validate. FFI-exposed (`verify_witness_cosig`) + fuzzed.
3. **The witness service** (`services/witness`, new) — an independent Ed25519
   key (first-boot generate + persist, like the operator key); `POST
   /witness/cosign` verifies the operator sig + `verify_consistency` from its
   last-co-signed size + strict-monotone `log_size`, then co-signs + advances its
   high-water; `GET /witness/key` publishes the verifying key.
4. **Operator integration** (`services/transparency`) — after signing a root,
   call the witness, persist + serve the co-sig on `GET /transparency/root`;
   publish the witness key.
5. **Client co-sig gate** (web / iOS / Android + FFI) — **landed.** Each client
   pins the witness key build-time (`VITE_LAKKO_TRANSPARENCY_WITNESS_KEY` /
   Info.plist `LakkoTransparencyWitnessKey` / `BuildConfig.LAKKO_TRANSPARENCY_WITNESS_KEY`),
   decodes the optional `witness_cosig_b64` from `GET /transparency/root`, and
   runs a `witnessGateRefuses` gate immediately after the no-rollback gate in BOTH
   `verifyIdentityInLog` + `verifyRotationChain`: with a pinned witness key, a root
   with NO co-sig OR a co-sig that fails `verifyWitnessCosig` (the FFI/wasm
   primitive over the served `(tree_root, log_size)`) is refused → surfaced as a
   `mismatch` (the root is untrusted, so any inclusion/chain proof against it is
   meaningless). With NO pinned witness key the gate is OFF (the operator-only
   transition posture). The gate is a test-seam (`verifyWitness` / `WitnessGate`)
   mirroring the existing inclusion/chain seams. Verified: web `tsc` + 63 vitest
   (9 new witness cases) + `vite build`; iOS `xcodebuild` BUILD + TEST BUILD
   SUCCEEDED (8 new `XCTest` cases); Android `:app:compileDebugKotlin` + the
   isolated `TransparencyVerifyTest` 43/0 (8 new cases).
6. **Extend the recovery-e2e** — a co-located witness container; assert a served
   root carries a valid co-sig + a simulated fork is refused. **Landed +
   verified firsthand.** `ci/recovery-e2e/` runs a 6-container stack
   (discovery + transparency + witness + 3 MariaDB); `recovery_kt_e2e.rs`
   step 6 asserts the served root carries a witness co-sig that verifies, and
   step 7 (Finding-2) POSTs a validly operator-signed but FORKED root (same
   `log_size`, different `tree_root`) to the live `POST /witness/cosign` and
   asserts a **409** refusal (`NotConsistent`), plus a **400** for an
   invalid-operator-signature root. Observed green against the live stack
   (`recovery_kt_pipeline_publish_log_fetch_verify_rotate` passes). No
   deferred items remain for ADR-0020.

## Consequences

- **Invariant #2 holds.** The witness co-sig + the consistency proof are public
  artefacts over public roots; no key material.
- **Invariant #4 holds + strengthens.** The root gate now needs operator AND
  witness; a missing/invalid co-sig fails closed.
- **Invariant #6 strengthens.** A rotation is anchored to a root that is now
  non-forkable without two-domain collusion.
- The witness is a NEW trust party; its independence is the load-bearing deploy
  property, and operator+witness collusion is the documented residual.

Gate this ADR + the audited-core Sprint 1/2 changes through `lakko-security`
before merge.
