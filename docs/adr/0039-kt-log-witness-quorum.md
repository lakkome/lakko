<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0039: KT-log witness quorum — K-of-N co-signing (G-30 finding 1 collusion residual)

- **Status:** Accepted (maintainer approved 2026-07-21; implemented S1–S5,
  2026-07-21 — the S5 2-of-3 witness-quorum e2e is built and CI/Docker-gated like
  the rest of `ci/recovery-e2e`, not run in the authoring environment). This ADR
  **extends** ADR-0020 (it does not supersede it — the single-witness mechanism
  and its Merkle-consistency defence stay verbatim in force); it realises the
  "K-of-N a config slot for later" ADR-0020 explicitly parked.
- **Date:** 2026-07-21
- **Context:** Phase 2 key transparency — the KT-log split-view residual.

## Context

ADR-0020 closed G-30 finding 1 (operator equivocation / split-view) **for the
single-witness model**: a client trusts an operator root only when it carries
BOTH the operator signature AND a valid co-signature from a build-time-pinned,
independent **witness**, and the witness co-signs a new root only after a Merkle
consistency proof (RFC 6962 §2.1.2) shows it append-only-extends its
last-co-signed root. Two divergent forks cannot both extend one root, so an
operator can no longer fork the log **alone** — a split-view now requires
operator **and** witness collusion, across two independent trust domains
(`docs/security/invariant-index.md:172`, C-57).

ADR-0020 named its residual and parked it explicitly:

> **Single witness first; K-of-N a config slot for later.** The residual is
> operator+witness collusion — documented; a witness threshold + the deferred
> client-gossip layer (the other carrier from the G-30 #1 design) are the later
> hardenings. (`docs/adr/0020-witness-cosigning.md:56-59`)

**This ADR is that config slot, made real.** With a single witness, an operator
that also controls (or compromises) that one witness can fork: it signs two
divergent valid roots AND gets both co-signed, and every client's local checks —
operator sig, no-rollback, witness cosig — pass on each fork. One witness is one
collusion partner. Requiring co-signatures from a **threshold K of N**
independently-pinned witnesses raises the bar from "corrupt one witness" to
"corrupt K witnesses."

**Everything this needs already exists and is audited — the gap is a threshold,
not a new mechanism.** The recon of the landed ADR-0020 code establishes:

- **The primitive is already key-parameterised.**
  `crates/lakko-transparency/src/witness.rs`
  `verify_witness_cosig(bytes, witness_key, expected_tree_root, expected_log_size)`
  (lines 99–135) takes the witness key as an argument and binds the cosig to
  `(tree_root, log_size)` — so a quorum is a thin loop over it counting distinct
  satisfied keys. The 105-byte blob
  (`version(1) ‖ tree_root(32) ‖ log_size(8 BE) ‖ sig(64)`, domain
  `lakko/transparency/witness/v1`, `witness.rs:34,39-41`) is unchanged.
- **The witness service is already per-instance.** `services/witness/` holds its
  own Ed25519 key (`witness_key.rs` — a single-row table, generated fresh on
  first boot, out-of-band-distributed, independent of the operator key), its own
  `(tree_root, log_size)` high-water (`store.rs:21-25` — "the only log state it
  keeps"), its own DB (`migrations/0001_witness.sql`), and pins the operator key
  (`LAKKO_WITNESS_OPERATOR_PUBKEY`). **N witnesses = N independent instances of
  the existing binary; the witness service needs no code change for K-of-N.**
- **Transparency co-signs-on-read against a SINGLE witness today.**
  `services/transparency/src/witness_client.rs` `from_env` reads one
  `LAKKO_TRANSPARENCY_WITNESS_URL` + one cosign secret (lines 51–64);
  `store.rs` `root_with_cosig` caches one `Witnessed` and serves `None` (never a
  stale cosig) when the witness is unreachable or refuses (lines 447–501);
  `handlers.rs` `RootRes` carries one `witness_cosig_b64: Option<String>`
  (line 245). K-of-N widens each of these to a list.
- **The three clients pin ONE key + run a fail-closed gate.** Web
  `VITE_LAKKO_TRANSPARENCY_WITNESS_KEY` (`apps/web/src/transparency/config.ts:77`),
  iOS `Info.plist` `LakkoTransparencyWitnessKey`, Android
  `BuildConfig.LAKKO_TRANSPARENCY_WITNESS_KEY`; the `witnessGateRefuses` gate
  runs immediately after the no-rollback gate in BOTH `verifyIdentityInLog` and
  `verifyRotationChain` (`apps/web/src/transparency/verify.ts:239,488,677`),
  fail-closed (missing/invalid → `mismatch`), and is OFF when no key is pinned
  (the operator-only transition posture). K-of-N: pin a key **set** + a threshold
  K, and require ≥ K distinct-key valid cosigs.
- **Deploy reality: even N=1 is not in production infra.** There is **no
  `lakko-svc-witness` systemd unit** and **no `LAKKO_WITNESS_*` /
  `LAKKO_TRANSPARENCY_WITNESS_*` in `infra/etc-lakko/lakko.env.example`** — a
  grep of `infra/` for "witness" is empty. The only place a witness runs is the
  co-located `ci/recovery-e2e/docker-compose.yml` (a single witness container,
  lines 56–130). So "N independently-operated witnesses" is **greenfield ops**,
  and — the load-bearing consequence for the wire design below — there are **zero
  clients pinned to a deployed single-witness key** to stay compatible with.

## The load-bearing honest caveat (state up front)

This must be stated precisely before the design, so the deliverable is not
mis-sold.

**This ADR builds the K-of-N _mechanism_. The collusion-resistance it buys is
only real once the N witnesses are _independently operated in distinct trust
domains_ — different operators / organisations / hosts / key custody. That is a
DEPLOY / OPS property, not code.** ADR-0020 already framed witness independence
as a deployment requirement, not a code-enforced one ("Independence is an OPS
property, not code-enforced", `docs/adr/0020-witness-cosigning.md:60-63`); a
threshold does not change that — it multiplies it.

Concretely: a **single operator that runs all N witnesses itself gains nothing
against operator+witness collusion** — the operator *is* every witness, so it can
mint all N cosigs over a forked root and satisfy any K. K-of-N raises the
collusion bar only to the extent that corrupting K of the N witnesses is harder
than corrupting one, and that is true only when the witnesses are genuinely
separate parties. Running N witnesses on one operator's own infrastructure is
security theatre against this specific threat.

So the deliverable is honest and bounded: **the threshold is a config slot, ready
in code and proven by a 2-of-3 end-to-end test; the collusion-resistance
materialises when independent witness operators exist.** This ADR must not claim
K-of-N *closes* the operator+witness collusion residual in a single-operator
deployment — it closes it only under independent operation. The residual is
recorded honestly (§ Consequences).

## Decision

Extend witness co-signing from a single witness to a **K-of-N witness quorum**: a
client trusts an operator root only when it carries the operator signature AND
**≥ K valid co-signatures from distinct keys in a build-time-pinned witness key
set** of size N. Enforcement stays **client-side and fail-closed**; the server
fans out to N witnesses best-effort and serves whatever cosigs it has.

### 1. Quorum primitive — `verify_witness_quorum`

A thin, audited-primitive-reusing verifier beside `verify_witness_cosig` in
`crates/lakko-transparency/src/witness.rs`:

```text
verify_witness_quorum(cosigs, pinned_keys, threshold, expected_tree_root, expected_log_size)
  -> Result<()>
```

- Iterate over the **pinned key set** (fixed at build time, distinct by
  construction — a pin is a *set*), and for each key ask "does **any** presented
  cosig verify under this key over `(expected_tree_root, expected_log_size)`?"
  via the existing `verify_witness_cosig`. Count the keys for which the answer is
  yes. Return `Ok` iff that count `>= threshold`.

- **Dedup-by-key is the load-bearing correctness property.** The count is over
  **distinct pinned keys satisfied**, never over presented cosigs. The cosig list
  is attacker-influenceable in transit (the zero-trust server, or a MITM, can
  duplicate one honest witness's cosig N times to *look* like a quorum, or replay
  an old cosig). Iterating over the pinned keys and counting each at most once
  makes that free: a duplicated / replayed cosig satisfies at most the one key it
  is signed by, and that key contributes exactly one to the count. So one
  witness's cosig can **never** satisfy the threshold twice, and a single
  compromised witness can never fake a K-of-N quorum by flooding the list. (This
  is the direct analogue of a CT / Sigsum witness-quorum policy: a root is
  trusted iff ≥ K *distinct* known witnesses attest it.)

- **Fail-closed on misconfiguration.**
  - `threshold == 0` → `Err` (a "0-of-N quorum" would trust any root — that is an
    operator-only posture masquerading as a quorum; refuse loudly rather than
    silently accept unwitnessed roots).
  - `threshold > pinned_keys.len()` → `Err` (an unsatisfiable threshold; it would
    make the client refuse *every* root and read as a mysterious outage — surface
    it as an explicit config error, not a silent permanent `mismatch`).
  - A bound on `cosigs.len()` (reject an over-long presented list before doing
    `O(|keys| × |cosigs|)` Ed25519 verifies) caps a server that floods the list
    to burn client CPU. Both `|keys|` (N witnesses) and the cap are small.

- Reuses `verify_witness_cosig` per (key × cosig), so the quorum **inherits** the
  `(tree_root, log_size)` binding, the domain separation
  (`lakko/transparency/witness/v1`, distinct from the operator root sig), and the
  Ed25519 verification verbatim. No new signing, no new wire blob, no new domain.

- **N = 1 / K = 1 is behaviourally identical to today** — a one-element pinned set
  + a one-element cosig list + threshold 1 reduces `verify_witness_quorum` to a
  single `verify_witness_cosig`. So the quorum generalises the single witness
  without changing its meaning.

Property tests: K distinct valid cosigs at threshold K → `Ok`; K−1 → refuse; a
single witness's cosig replayed N times counts once (→ refuse below K); duplicate
pinned keys / duplicate cosigs collapse to one; `threshold == 0` refused;
`threshold > N` refused; a cosig over the wrong `(tree_root, log_size)` never
counts; N=1/K=1 equals `verify_witness_cosig`. A `verify_witness_quorum` fuzz
target in `crates/lakko-transparency/fuzz` mirroring the existing
`verify_witness_cosig` target (the cosig list + key set are attacker-controlled
bytes a malicious serving tier can craft — the loop, the dedup, and the bounds
must stay panic-free).

### 2. Wire = a repeated field, a CLEAN REPLACE (not additive)

`RootRes.witness_cosig_b64: Option<String>` → `witness_cosigs_b64: Vec<String>`
(a base64 cosig per configured witness that co-signed the served root). The three
clients decode the list and hand it to `verify_witness_quorum`.

**This is a clean replace, not a dual-field additive change, and the reason is
the deploy reality above.** Because **no witness is deployed in production — not
even N=1** (no systemd unit, no `LAKKO_TRANSPARENCY_WITNESS_*` in
`lakko.env.example`; only `ci/recovery-e2e` runs one), there are **zero clients
in the field pinned to a single-witness key** and therefore **no backward-compat
burden**. Carrying both `witness_cosig_b64` and `witness_cosigs_b64` (the recon's
"additive" option) would be dead weight preserving compatibility with a wire
shape nothing consumes. So we replace the singular field with the plural one
cleanly — simpler, one code path, no legacy field to deprecate later. (Had a
single-witness deployment existed in the field, the additive path would be
mandatory; it does not, so it is not.)

### 3. Fan-out + availability posture — best-effort collect, client-enforced threshold

The transparency co-sign-on-read model is preserved and widened:

- `witness_client.rs` reads a **list** of witness endpoints + per-witness cosign
  secrets (e.g. `LAKKO_TRANSPARENCY_WITNESS_URLS` +
  `LAKKO_TRANSPARENCY_WITNESS_COSIGN_SECRETS`, comma-separated / indexed) and
  builds one `WitnessClient` per configured witness. An empty list → the
  integration is OFF (the existing transition fallback: serve roots with an empty
  cosig list; clients with no pinned set stay operator-only).

- `root_with_cosig` fans out to the N witnesses **best-effort**: it asks each for
  a cosig over the fresh head root (handing each its consistency proof from that
  witness's own last-co-signed high-water). A witness that is **down / slow /
  refuses (409 fork) contributes nothing this round** — it is simply absent from
  the served bundle. The server **never blocks or waits** on a laggard and
  **never serves a stale cosig** (the ADR-0020 per-witness invariant — a cosig is
  served only when it binds the current head `(tree_root, log_size)`, else `None`
  for that witness). The result is a **bundle** of the cosigs it could collect,
  cached keyed on `(tree_root, log_size)` and re-collected on log growth.

- **The client is the enforcement point.** It counts distinct-key valid cosigs
  against its pinned set and **fails closed if `< K`** (`mismatch`). The server is
  zero-trust and could serve a sub-threshold bundle (or none); only the client,
  holding the pinned key set + threshold, can decide the quorum is met. So a
  transient shortfall (say 2 of 3 witnesses reachable, K = 2 → still trusted;
  K = 3 → the client refuses until the third catches up) is a client-side
  availability trade-off the operator cannot override.

- **Persistence widens** from the single-row `witnessed_root`
  (`migrations/0003_witnessed_root.sql`) to a **per-witness** cache of
  `(witness_id, tree_root, log_size, cosig)` (migration `0005`), so a restart
  restores the whole bundle without re-collecting from scratch. `services/witness`
  is **untouched** (each instance already stores only its own high-water).

- **v1 re-collects on log growth** (the existing trigger). "Retry a
  transiently-down witness *between* growths" — so a witness that was unreachable
  when the head last grew can still contribute before the next growth, tightening
  availability for a high-K policy — is a **noted refinement, not v1** (§ Open
  questions).

### 4. Off-by-default + staged rollout — no flag-day, both directions

The whole integration stays dormant unless keys are pinned. Each client pins a
**key set** + a **threshold** (default-empty / unset → the gate is OFF →
operator-only posture, exactly as the single-key pin is today):

| Platform | Pin |
|---|---|
| Web | `VITE_LAKKO_TRANSPARENCY_WITNESS_KEYS` (set) + `VITE_LAKKO_TRANSPARENCY_WITNESS_THRESHOLD` |
| iOS | `Info.plist` `LakkoTransparencyWitnessKeys` + `LakkoTransparencyWitnessThreshold` |
| Android | `BuildConfig.LAKKO_TRANSPARENCY_WITNESS_KEYS` + `LAKKO_TRANSPARENCY_WITNESS_THRESHOLD` |

Rollout order (staged, so neither direction is a flag-day): **first** deploy N
witnesses (N independent instances of the existing binary, ideally in distinct
trust domains — § caveat) + the list-serving transparency; **then** ship clients
pinning the set + threshold. Until a client pins a set it stays operator-only;
until the server serves cosigs the pinned-set clients fail closed (which is
correct — a client that has been told to demand a quorum should refuse a root
with none). A single-key/threshold-1 pin reproduces today's single-witness
behaviour exactly, so an existing (test) pin migrates trivially.

## Alternatives

- **Keep the single witness (do nothing).** Rejected — it leaves the
  operator+witness collusion residual, which is the entire point of this ADR. A
  single witness is a single collusion partner; one compromise re-opens the
  split-view.

- **An additive dual-field wire (`witness_cosig_b64` + `witness_cosigs_b64`).**
  Rejected as unnecessary. The additive shape exists to keep old field-pinned
  clients working across a wire change; **there are no deployed witness clients**
  (§ Context / § Decision 2 — no production witness, so no client pins a
  single-witness key in the field), so there is nothing to stay compatible with.
  A clean replace to the repeated field is simpler and leaves no legacy field to
  carry. (This is the deliberate inverse of a mid-deployment migration — the
  greenfield state is what licenses the clean cut.)

- **Server-side threshold enforcement (the server refuses to serve a
  sub-threshold root).** Rejected. The server is zero-trust; a malicious /
  colluding operator could serve a sub-threshold bundle and *claim* it is a
  quorum, or drop cosigs selectively. The **client** must be the enforcement
  point — it holds the pinned key set + threshold, and it fails closed on
  `< K` (invariant #4). The server's role is transport + best-effort collection
  only; it never decides trust. (The server may *log / meter* the quorum health
  for ops visibility, but that is diagnostics, not enforcement.)

- **N-of-N (unanimous) instead of K-of-N.** Rejected — it makes **any single
  witness a single point of availability failure**: one witness down or slow
  stalls trust for every client. K-of-N tolerates `N − K` unavailable witnesses
  while still demanding K colluders to fork, which is the availability/security
  balance ADR-0020's "config slot" implied. K is the operator's dial (higher K =
  stronger, less available).

- **A client-gossip / split-view cross-check layer** (clients compare the roots
  they each observe out-of-band, so a fork is caught even without a witness
  quorum). This is the *other* carrier ADR-0020's G-30 #1 design parked
  alongside the witness threshold, and a strictly stronger hardening (it does not
  trust the witnesses at all). **Out of scope here** — it needs a client-to-client
  gossip transport and an anonymity/metadata analysis that this ADR does not
  open. Noted as a future direction; the quorum and gossip are complementary, not
  exclusive.

## Consequences

- **Positive.** The operator+witness collusion residual ADR-0020 parked becomes
  addressable: a fork now requires corrupting **K** independently-operated
  witnesses instead of one. The change is small and reuses the audited primitive
  end-to-end — no new signing, no new wire blob, no new domain separator, no
  witness-service code change; the `verify_witness_cosig` binding + domain
  separation carry through the quorum unchanged. N=1/K=1 is byte-for-byte today's
  behaviour, so the single-witness path is not regressed. The clean wire replace
  (licensed by the no-deployed-witness greenfield state) keeps one code path.

- **Negative / new risks.** N witnesses to **operate independently** — the
  load-bearing OPS property (§ caveat): the code cannot enforce that the N
  witnesses are distinct trust domains, and a single-operator N-witness
  deployment buys nothing against this threat. A higher K trades availability for
  security (K reachable witnesses required to trust a root; a shortfall fails
  closed → a client cannot verify identities until the quorum recovers) — the
  operator's dial to set consciously. The fan-out adds N round-trips per log
  growth (bounded, off the append hot path — appends never block on it, as today).
  A per-witness cache + migration `0005` widens the persisted state.

- **Neutral.** Off-by-default and staged both directions (no flag-day): an
  unpinned client stays operator-only; a pinned client fails closed until the
  server serves ≥ K. The threshold + key set are the only new config; the wire
  gains a repeated field.

- **The residual, stated plainly.** K-of-N raises the collusion bar from one
  witness to K, **but only under independent witness operation**. In a
  single-operator deployment (the operator runs all N witnesses), operator+all-N
  collusion is trivial and this ADR closes nothing — it is *mechanism ready,
  guarantee pending independent operators*. The stronger, witness-trust-free
  hardening (client gossip / cross-check) remains the deferred next step
  (§ Alternatives). Both are recorded honestly against G-30 finding 1 and C-57 in
  the S5 reconcile.

### Invariants touched

- **#2 (server sees only ciphertext + public artefacts) — holds.** The cosig
  bundle + the consistency proofs are public artefacts over public roots; no key
  material. The clients hold no secret (verify-only); the pinned witness keys are
  public verifying keys.
- **#4 (no silent downgrade / fail-closed) — strengthened.** The root gate now
  requires the operator signature AND ≥ K distinct-key witness cosigs; a
  sub-threshold or absent bundle fails closed (`mismatch`), and the quorum
  primitive itself fails closed on a `threshold == 0` / `threshold > N`
  misconfig.
- **#5 (social graph not on the server) — holds.** Each witness persists only
  `(tree_root, log_size)` high-water, never accounts (`services/witness/store.rs`,
  unchanged); the transparency per-witness cache holds only roots + cosigs.
- **#6 (identity rotation only via a KT-log supersession proof) — strengthened.**
  A rotation is anchored to a root that is now non-forkable without K-of-N
  collusion (the `verifyRotationChain` witness gate demands the quorum).
- **Invariant-index touchpoint (S5, deferred to the reconcile).** This extends the
  existing **C-57** (witness co-signing) from single-witness to K-of-N — it is a
  strengthening of an existing indexed invariant, **not a new C-number** — and it
  addresses (under the independence caveat) the C-57 / G-30 #1 residual
  "operator+witness collusion (K-of-N a config slot for later)". The
  `docs/security/invariant-index.md` C-57 wording, the `docs/security/known-gaps.md`
  G-30 note, the CLAUDE.md ADR-table row + module-map, and the
  `ci/recovery-e2e` 2-of-3 assertion are all S5 (this ADR does not touch them).

A `lakko-security` pass is warranted before merge (maintainer-invoked, per the
crypto/MLS/transport/auth gate): it touches the audited `lakko-transparency`
witness primitive (a new `verify_witness_quorum`), the fail-closed client root
gate, and invariants #2 / #4 / #6 — mainly to confirm the dedup-by-key counting
(no cosig or key double-counts toward K), the `threshold == 0` / `threshold > N`
fail-closed misconfig handling, and that the clean wire replace introduces no
path that accepts a root on fewer than K distinct valid cosigs.

## Open questions

- **Retry a transiently-down witness between log growths.** v1 re-collects cosigs
  only on log growth (the existing trigger), so a witness unreachable at the last
  growth stays absent from the bundle until the next append. A background retry
  loop (re-poll a missing witness for the *current* head between growths) would
  tighten availability for a high-K policy. Deferred — a refinement, not
  correctness (the client already fails closed on a shortfall, so this only
  affects *when* trust recovers, never whether a fork slips through).
- **K-of-N policy expression on the wire vs. client-only.** v1 pins K + the key
  set purely on the client (the server never learns or enforces K). A future
  signed-config-delivered witness policy (ADR-0026-style bootstrap) could rotate
  the witness set / threshold without a client rebuild — deferred, the same
  posture as the ADR-0038 "pin-only vs. signed-config key rotation" open question.
- **Witness key rotation within the set.** Rotating one of the N witness keys is a
  client-rebuild event today (build-time pin). Whether to allow an in-set rotation
  via signed config (above) or to require a coordinated rebuild is a
  release-engineering call, deferred.
- **Client gossip / cross-check as the stronger hardening.** The witness-trust-free
  split-view defence ADR-0020 also parked (§ Alternatives) remains the next step
  beyond the quorum; whether/when to build it (and its metadata-privacy analysis)
  is a separate future ADR.
- **Independent-operator recruitment.** The guarantee is pending real independent
  witness operators (§ caveat) — an ops / partnerships question, not a design gap,
  tracked as the condition under which K-of-N's collusion-resistance becomes real.

## Sprint decomposition

Each step names the ADR-0020 file it mirrors / widens.

- **S1 — quorum primitive + property tests + fuzz.** Add `verify_witness_quorum`
  to `crates/lakko-transparency/src/witness.rs` beside `verify_witness_cosig`
  (dedup-by-pinned-key counting, `threshold == 0` / `threshold > N` /
  over-long-list fail-closed guards, reusing `verify_witness_cosig` per cosig).
  Property tests (K-valid → Ok; K−1 → refuse; replayed/duplicate cosig counted
  once; misconfig thresholds refused; wrong `(tree_root, log_size)` never counts;
  N=1/K=1 ≡ `verify_witness_cosig`). Fuzz target `verify_witness_quorum` in
  `crates/lakko-transparency/fuzz` mirroring the `verify_witness_cosig` target.
- **S2 — FFI export.** Expose `verify_witness_quorum` on UniFFI (Swift/Kotlin,
  `crates/lakko-ffi-uniffi/src/lib.rs`) + wasm
  (`crates/lakko-ffi-wasm/src/supersession.rs`, `verifyWitnessQuorum`),
  verify-only, mirroring the existing `verify_witness_cosig` exposure. No sign on
  the FFI (signing stays in `services/witness`).
- **S3 — transparency N-witness fan-out + repeated wire field + cache/persist
  widen + migration.** `witness_client.rs`: build one `WitnessClient` per
  configured witness from a list env (`LAKKO_TRANSPARENCY_WITNESS_URLS` +
  `_COSIGN_SECRETS`), empty → OFF. `store.rs` `root_with_cosig`: best-effort
  collect a bundle (each witness handed its own consistency proof), never
  blocking, never stale-per-witness; per-witness cache keyed on
  `(tree_root, log_size)`. `handlers.rs` `RootRes.witness_cosigs_b64: Vec<String>`
  (clean replace of the singular field). Migration `0005` widens `witnessed_root`
  (single-row) to a per-witness cosig set. **`services/witness` unchanged.**
- **S4 — 3-client key-set + threshold pin + repeated wire decode + quorum gate.**
  Pin a witness key **set** + threshold K per platform (web
  `VITE_LAKKO_TRANSPARENCY_WITNESS_KEYS` + `_THRESHOLD`, iOS
  `LakkoTransparencyWitnessKeys` + `…Threshold`, Android
  `LAKKO_TRANSPARENCY_WITNESS_KEYS` + `_THRESHOLD`, all default-empty → gate OFF).
  Decode the `witness_cosigs_b64` list; change `witnessGateRefuses` (in both
  `verifyIdentityInLog` + `verifyRotationChain`) to require ≥ K distinct-key valid
  cosigs via `verify_witness_quorum`. N=1/K=1 reproduces today's gate.
- **S5 — docs reconcile.** Flip this ADR's Status note as landed; add the
  CLAUDE.md ADR-table row + module-map update (`lakko-transparency` gains
  `verify_witness_quorum`; both FFI rows gain the quorum verify surface; the fuzz
  list gains `verify_witness_quorum`); update **C-57** in
  `docs/security/invariant-index.md` (single-witness → K-of-N, residual now
  "operator+K-witness collusion, only under independent operation") + the
  cross-reference in ADR-0020's "K-of-N a config slot for later" line (pointing at
  this ADR) + the **G-30 #1** note in `docs/security/known-gaps.md`; extend
  `ci/recovery-e2e` to a **2-of-3** witness quorum (three witness containers, a
  served root carrying ≥ 2 valid cosigs; a simulated shortfall of 1 → the client
  gate refuses at K = 2). This ADR does **not** touch any of these (S5 is a later
  sprint).

## References

- **ADR-0020** — witness co-signing for KT-log split-view resistance. This ADR
  *extends* it (does not supersede): the Merkle-consistency defence, the 105-byte
  cosig blob, the domain separator, the co-sign-on-read model, and the fail-closed
  client gate all carry through unchanged. Realises its parked "K-of-N a config
  slot for later" (`0020-witness-cosigning.md:56-59`).
- **ADR-0019** — client-authorised log leaf (G-30 #2); the genesis self-sig +
  rotation chain the witness gate composes with.
- **ADR-0016** — identity-supersession transport; the equivocation-resistance
  framing this quorum advances.
- **ADR-0038** — the recent `lakko-attest` ADR whose default-empty-pin /
  fail-toward-safe / staged-rollout discipline and honest-residual voice this ADR
  mirrors.
- **RFC 6962** §2.1.2 — Merkle consistency proofs (the per-witness fork defence,
  unchanged from ADR-0020).
- **RFC 8032** — Ed25519 signatures (the cosig + operator root sig).
- The **Sigsum / Certificate-Transparency witness-quorum** model — a log root is
  trusted iff ≥ K distinct known witnesses cosign it; the dedup-by-known-key
  counting in § Decision 1 is the direct analogue.
